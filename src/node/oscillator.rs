use std::f32::consts::PI;
use std::fmt::Debug;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use crate::alloc::ChannelData;
use crate::buffer::{ChannelConfig, ChannelConfigOptions};
use crate::context::{AsBaseAudioContext, AudioContextRegistration, AudioParamId};
use crate::control::Scheduler;
use crate::param::{AudioParam, AudioParamOptions};
use crate::process::{AudioParamValues, AudioProcessor};
use crate::SampleRate;

use crossbeam_channel::{self, Receiver, Sender};
use lazy_static::lazy_static;

use super::{AudioNode, AudioScheduledSourceNode};

const TABLE_LENGTH_USIZE: usize = 2048;
const TABLE_LENGTH_F32: f32 = TABLE_LENGTH_USIZE as f32;

// Compute one period sine wavetable of size TABLE_LENGTH
lazy_static! {
    static ref SINETABLE: Vec<f32> = {
        let table: Vec<f32> = (0..TABLE_LENGTH_USIZE)
            .map(|x| ((x as f32) * 2.0 * PI * (1. / (TABLE_LENGTH_F32))).sin())
            .collect();
        table
    };
}

/// Options for constructing a periodic wave
pub struct PeriodicWaveOptions {
    /// The real parameter represents an array of cosine terms of Fourrier series.
    ///
    /// The first element (index 0) represents the DC-offset.
    /// This offset has to be given but will not be taken into account
    /// to build the custom periodic waveform.
    ///
    /// The following elements (index 1 and more) represent the fundamental and harmonics of the periodic waveform.
    pub real: Vec<f32>,
    /// The imag parameter represents an array of sine terms of Fourrier series.
    ///
    /// The first element (index 0) will not be taken into account
    /// to build the custom periodic waveform.
    ///
    /// The following elements (index 1 and more) represent the fundamental and harmonics of the periodic waveform.
    pub imag: Vec<f32>,
    /// By default PeriodicWave is build with normalization enabled (disable_normalization = false).
    /// In this case, a peak normalization is applied to the given custom periodic waveform.
    ///
    /// If disable_normalization is enabled (disable_normalization = true), the normalization is
    /// defined by the periodic waveform characteristics (img, and real fields).
    pub disable_normalization: bool,
}

/// PeriodicWave is a setup struct required to build
/// custom periodic waveform oscillator type.
#[derive(Debug, Clone)]
pub struct PeriodicWave {
    /// The real parameter represents an array of cosine terms of Fourrier series.
    ///
    /// The first element (index 0) represents the DC-offset.
    /// This offset has to be given but will not be taken into account
    /// to build the custom periodic waveform.
    ///
    /// The following elements (index 1 and more) represent the fundamental and harmonics of the periodic waveform.
    real: Vec<f32>,
    /// The imag parameter represents an array of sine terms of Fourrier series.
    ///
    /// The first element (index 0) will not be taken into account
    /// to build the custom periodic waveform.
    ///
    /// The following elements (index 1 and more) represent the fundamental and harmonics of the periodic waveform.
    imag: Vec<f32>,
    /// By default PeriodicWave is build with normalization enabled (disable_normalization = false).
    /// In this case, a peak normalization is applied to the given custom periodic waveform.
    ///
    /// If disable_normalization is enabled (disable_normalization = true), the normalization is
    /// defined by the periodic waveform characteristics (img, and real fields).
    disable_normalization: bool,
}

impl PeriodicWave {
    /// Returns a PeriodicWave
    ///
    /// # Arguments
    ///
    /// * `real` - The real parameter represents an array of cosine terms of Fourrier series.
    /// * `imag` - The imag parameter represents an array of sine terms of Fourrier series.
    /// * `constraints` - The constraints parameter specifies the normalization mode of the PeriodicWave
    ///
    /// # Example
    ///
    /// ```no_run
    ///    use web_audio_api::context::{AudioContext, AsBaseAudioContext};
    ///    use web_audio_api::node::{PeriodicWave, PeriodicWaveOptions};
    ///
    ///    let context = AudioContext::new();
    ///
    ///    let options = PeriodicWaveOptions {
    ///    real: vec![0.,1.,1.],
    ///    imag: vec![0.,0.,0.],
    ///    disable_normalization: false,
    ///    };
    ///
    ///    let periodic_wave = PeriodicWave::new(&context, Some(options));
    /// ```
    ///
    pub fn new<C: AsBaseAudioContext>(_context: &C, options: Option<PeriodicWaveOptions>) -> Self {
        if let Some(PeriodicWaveOptions {
            real,
            imag,
            disable_normalization,
        }) = options
        {
            // Todo: assertions not complete. Missing assertions when
            // only real field is specified, anly imag field is specified,
            // and neither real or imag is specified
            assert!(
                real.len() >= 2,
                "RangeError: Real field length should be at least 2"
            );
            assert!(
                imag.len() >= 2,
                "RangeError: Imag field length should be at least 2",
            );
            assert!(
                real.len() == imag.len(),
                "RangeError: Imag and real field length should be equal"
            );
            Self {
                real,
                imag,
                disable_normalization,
            }
        } else {
            Self {
                real: vec![0., 0.],
                imag: vec![0., 1.],
                disable_normalization: false,
            }
        }
    }
}

/// Options for constructing an OscillatorNode
#[derive(Debug)]
pub struct OscillatorOptions {
    pub type_: OscillatorType,
    pub frequency: f32,
    pub detune: f32,
    pub channel_config: ChannelConfigOptions,
    pub periodic_wave: Option<PeriodicWave>,
}

impl Default for OscillatorOptions {
    fn default() -> Self {
        Self {
            type_: OscillatorType::default(),
            frequency: 440.,
            detune: 0.,
            channel_config: ChannelConfigOptions::default(),
            periodic_wave: None,
        }
    }
}

/// Waveform of an oscillator
#[derive(Debug, Copy, Clone, PartialEq)]
pub enum OscillatorType {
    Sine,
    Square,
    Sawtooth,
    Triangle,
    Custom,
}

impl Default for OscillatorType {
    fn default() -> Self {
        OscillatorType::Sine
    }
}

impl From<u32> for OscillatorType {
    fn from(i: u32) -> Self {
        use OscillatorType::*;

        match i {
            0 => Sine,
            1 => Square,
            2 => Sawtooth,
            3 => Triangle,
            4 => Custom,
            _ => unreachable!(),
        }
    }
}

enum OscMsg {
    PeriodicWaveMsg(PeriodicWave),
}

/// Audio source generating a periodic waveform
pub struct OscillatorNode {
    registration: AudioContextRegistration,
    channel_config: ChannelConfig,
    frequency: AudioParam,
    detune: AudioParam,
    type_: Arc<AtomicU32>,
    scheduler: Scheduler,
    sender: Sender<OscMsg>,
}

impl AudioScheduledSourceNode for OscillatorNode {
    fn scheduler(&self) -> &Scheduler {
        &self.scheduler
    }
}

impl AudioNode for OscillatorNode {
    fn registration(&self) -> &AudioContextRegistration {
        &self.registration
    }

    fn channel_config_raw(&self) -> &ChannelConfig {
        &self.channel_config
    }

    fn number_of_inputs(&self) -> u32 {
        0
    }
    fn number_of_outputs(&self) -> u32 {
        1
    }
}

impl OscillatorNode {
    /// Returns an OscillatorNode
    ///
    /// # Arguments:
    ///
    /// * `context` - The AudioContext
    /// * `options` - The Oscillatoroptions
    pub fn new<C: AsBaseAudioContext>(context: &C, options: OscillatorOptions) -> Self {
        context.base().register(move |registration| {
            let sample_rate = context.base().sample_rate().0 as f32;
            let nyquist = sample_rate / 2.;

            // frequency audio parameter
            let freq_param_opts = AudioParamOptions {
                min_value: -nyquist,
                max_value: nyquist,
                default_value: 440.,
                automation_rate: crate::param::AutomationRate::A,
            };
            let (f_param, f_proc) = context
                .base()
                .create_audio_param(freq_param_opts, registration.id());
            f_param.set_value(options.frequency);

            // detune audio parameter
            let det_param_opts = AudioParamOptions {
                min_value: -153600.,
                max_value: 153600.,
                default_value: 0.,
                automation_rate: crate::param::AutomationRate::A,
            };
            let (det_param, det_proc) = context
                .base()
                .create_audio_param(det_param_opts, registration.id());
            det_param.set_value(options.detune);

            let type_ = Arc::new(AtomicU32::new(options.type_ as u32));
            let scheduler = Scheduler::new();
            let renderer = OscRendererInner::new(
                options.frequency,
                sample_rate,
                options.periodic_wave.clone(),
            );

            let (sender, receiver) = crossbeam_channel::bounded(0);

            let render = OscillatorRenderer {
                type_: type_.clone(),
                frequency: f_proc,
                detune: det_proc,
                scheduler: scheduler.clone(),
                renderer,
                receiver,
            };
            let node = OscillatorNode {
                registration,
                channel_config: options.channel_config.into(),
                frequency: f_param,
                detune: det_param,
                type_,
                scheduler,
                sender,
            };

            (node, Box::new(render))
        })
    }

    /// Returns the oscillator frequency audio parameter
    pub fn frequency(&self) -> &AudioParam {
        &self.frequency
    }

    pub fn detune(&self) -> &AudioParam {
        &self.detune
    }

    /// Returns the oscillator type
    pub fn type_(&self) -> OscillatorType {
        self.type_.load(Ordering::SeqCst).into()
    }

    /// set the oscillator type
    pub fn set_type(&self, type_: OscillatorType) {
        self.type_.store(type_ as u32, Ordering::SeqCst);
    }

    /// set the oscillator type to custom. The oscillator will generate
    /// a perdioc waveform following the PeriodicWave characteristics
    //
    //  TODO: The current implementation doesn't communicate its state
    //  to the OscillatorRenderer, and so has no effect on the rendering.
    //  This function should send the updated periodics waveform characteristics
    //  to the OscillatorRenderer and more specifically to the CustomRenderer
    pub fn set_periodic_wave(&mut self, periodic_wave: PeriodicWave) {
        self.set_type(OscillatorType::Custom);
        self.sender
            .send(OscMsg::PeriodicWaveMsg(periodic_wave))
            .expect("Sending periodic wave to the node renderer failed");
    }
}

struct OscillatorRenderer {
    type_: Arc<AtomicU32>,
    frequency: AudioParamId,
    detune: AudioParamId,
    scheduler: Scheduler,
    renderer: OscRendererInner,
    receiver: Receiver<OscMsg>,
}

impl AudioProcessor for OscillatorRenderer {
    fn process(
        &mut self,
        _inputs: &[crate::alloc::AudioBuffer],
        outputs: &mut [crate::alloc::AudioBuffer],
        params: AudioParamValues,
        timestamp: f64,
        _sample_rate: SampleRate,
    ) {
        // single output node
        let output = &mut outputs[0];

        // re-use previous buffer
        output.force_mono();

        // todo, sub-quantum start/stop
        if !self.scheduler.is_active(timestamp) {
            output.make_silent();
            return;
        }

        let freq_values = params.get(&self.frequency);

        let det_values = params.get(&self.detune);

        let mut computed_freqs: [f32; 128] = [0.; 128];

        if det_values
            .windows(2)
            .all(|w| (w[0] - w[1]).abs() < 0.000001)
        {
            let d = 2f32.powf(det_values[0] / 1200.);
            for (i, f) in freq_values.iter().enumerate() {
                computed_freqs[i] = f * d
            }
        } else {
            for (i, (f, d)) in freq_values.iter().zip(det_values).enumerate() {
                computed_freqs[i] = f * 2f32.powf(d / 1200.);
            }
        }

        let type_ = self.type_.load(Ordering::SeqCst).into();

        let buffer = output.channel_data_mut(0);

        // check if any message was send from the control thread
        if let Ok(msg) = self.receiver.try_recv() {
            match msg {
                OscMsg::PeriodicWaveMsg(p_w) => self.renderer.set_periodic_wave(p_w),
            }
        }

        self.renderer
            .generate_output(type_, buffer, &computed_freqs[..]);
    }

    fn tail_time(&self) -> bool {
        true
    }
}

struct SineState {
    interpol_ratio: f32,
    first: bool,
}

struct TriangleState {
    last_output: f32,
}

struct PeriodicState {
    cplxs: Vec<(f32, f32)>,
    norms: Vec<f32>,
    phases: Vec<f32>,
    incr_phases: Vec<f32>,
    interpol_ratios: Vec<f32>,
    norm_factor: Option<f32>,
    disable_normalization: bool,
    wavetable_state: WavetableState,
}

struct WavetableState {
    periodic_wavetable: Vec<f32>,
    phase: f32,
    incr_phase: f32,
    ref_freq: f32,
}

struct OscRendererInner {
    computed_freq: f32,
    sample_rate: f32,
    phase: f32,
    incr_phase: f32,
    sine: SineState,
    triangle: TriangleState,
    periodic: PeriodicState,
}

impl OscRendererInner {
    fn new(computed_freq: f32, sample_rate: f32, periodic_wave: Option<PeriodicWave>) -> Self {
        let incr_phase = computed_freq / sample_rate;
        let interpol_ratio = (incr_phase - incr_phase.floor()) * TABLE_LENGTH_F32;

        let PeriodicWave {
            real,
            imag,
            disable_normalization,
        } = if let Some(p_w) = periodic_wave {
            p_w
        } else {
            PeriodicWave {
                real: vec![0., 1.],
                imag: vec![0., 0.],
                disable_normalization: false,
            }
        };

        let cplxs: Vec<(f32, f32)> = real.iter().zip(&imag).map(|(&r, &i)| (r, i)).collect();

        let norms: Vec<f32> = cplxs
            .iter()
            .map(|(r, i)| (f32::powi(*r, 2i32) + f32::powi(*i, 2i32)).sqrt())
            .collect();

        let mut phases: Vec<f32> = cplxs
            .iter()
            .map(|(r, i)| {
                let phase = f32::atan2(*i, *r);
                if phase < 0. {
                    (phase + 2. * PI) * (TABLE_LENGTH_F32 / (2.0 * PI))
                } else {
                    phase * (TABLE_LENGTH_F32 / 2.0 * PI)
                }
            })
            .collect();

        let incr_phases: Vec<f32> = cplxs
            .iter()
            .enumerate()
            .map(|(idx, _)| TABLE_LENGTH_F32 * idx as f32 * (computed_freq / sample_rate))
            .collect();

        let interpol_ratios: Vec<f32> = incr_phases
            .iter()
            .map(|incr_phase| incr_phase - incr_phase.floor())
            .collect();

        let mut periodic_wavetable = Vec::with_capacity(2048);

        Self::generate_wavetable(
            &mut phases,
            &incr_phases,
            &interpol_ratios,
            &norms,
            &mut periodic_wavetable,
        );

        let norm_factor = if !disable_normalization {
            let norm_factor = Self::init_norm_factor(&periodic_wavetable);
            Some(norm_factor)
        } else {
            None
        };

        Self {
            computed_freq,
            sample_rate,
            phase: 0.0,
            incr_phase,
            sine: SineState {
                interpol_ratio,
                first: true,
            },
            triangle: TriangleState { last_output: 0.0 },
            periodic: PeriodicState {
                cplxs,
                norms,
                phases,
                incr_phases,
                interpol_ratios,
                norm_factor,
                disable_normalization,
                wavetable_state: WavetableState {
                    periodic_wavetable,
                    phase: 0.,
                    incr_phase: 1.,
                    ref_freq: computed_freq,
                },
            },
        }
    }

    fn set_periodic_wave(&mut self, periodic_wave: PeriodicWave) {
        let PeriodicWave {
            real,
            imag,
            disable_normalization,
        } = periodic_wave;
        // clear buffers
        self.periodic.cplxs.clear();
        self.periodic.norms.clear();
        self.periodic.phases.clear();
        self.periodic.incr_phases.clear();
        self.periodic.interpol_ratios.clear();

        // update cplxs
        for cplx in real.into_iter().zip(imag) {
            self.periodic.cplxs.push(cplx);
        }

        for (idx, (real, img)) in self.periodic.cplxs.iter().enumerate() {
            // update norms
            self.periodic
                .norms
                .push((f32::powi(*real, 2i32) + f32::powi(*img, 2i32)).sqrt());

            // update phases
            let phase = f32::atan2(*img, *real);
            if phase < 0. {
                self.periodic
                    .phases
                    .push((phase + 2. * PI) * (TABLE_LENGTH_F32 / (2.0 * PI)));
            } else {
                self.periodic
                    .phases
                    .push(phase * (TABLE_LENGTH_F32 / 2.0 * PI));
            }

            // update incr_phases
            self.periodic
                .incr_phases
                .push(TABLE_LENGTH_F32 * idx as f32 * (self.computed_freq / self.sample_rate));
        }

        // update interpol_ratios
        for incr_phase in &self.periodic.incr_phases {
            self.periodic
                .interpol_ratios
                .push((incr_phase - incr_phase.round()).abs());
        }

        // update wavetable
        self.update_wavetable();

        self.periodic.wavetable_state.ref_freq = self.computed_freq;

        // update norm_factor
        if !disable_normalization {
            self.update_norm_factor();
        } else {
            self.periodic.norm_factor = None;
        }
    }

    fn compute_params(&mut self, type_: OscillatorType, computed_freq: f32) {
        // No need to compute if frequency has not changed
        if type_ == OscillatorType::Sine {
            if self.sine.first {
                self.sine.first = false;
                self.incr_phase = computed_freq / self.sample_rate * TABLE_LENGTH_F32;
            }
            if (self.computed_freq - computed_freq).abs() < 0.01 {
                return;
            }
            self.computed_freq = computed_freq;
            self.incr_phase = computed_freq / self.sample_rate * TABLE_LENGTH_F32;
        }
        if (self.computed_freq - computed_freq).abs() < 0.01 {
            return;
        }
        self.computed_freq = computed_freq;
        self.incr_phase = computed_freq / self.sample_rate;
    }

    fn compute_periodic_params(&mut self, new_comp_freq: f32) {
        // No need to compute if frequency has not changed
        if (self.computed_freq - new_comp_freq).abs() < 0.01 {
            return;
        }

        for incr_phase in &mut self.periodic.incr_phases {
            *incr_phase *= new_comp_freq / self.computed_freq;
        }

        for (r, incr_ph) in self
            .periodic
            .interpol_ratios
            .iter_mut()
            .zip(self.periodic.incr_phases.iter())
        {
            *r = incr_ph - incr_ph.floor();
        }

        self.periodic.wavetable_state.incr_phase =
            new_comp_freq / self.periodic.wavetable_state.ref_freq;
        self.computed_freq = new_comp_freq;
    }

    fn generate_output(
        &mut self,
        type_: OscillatorType,
        buffer: &mut ChannelData,
        freq_values: &[f32],
    ) {
        match type_ {
            OscillatorType::Sine => self.generate_sine(type_, buffer, freq_values),
            OscillatorType::Square => self.generate_square(type_, buffer, freq_values),
            OscillatorType::Sawtooth => self.generate_sawtooth(type_, buffer, freq_values),
            OscillatorType::Triangle => self.generate_triangle(type_, buffer, freq_values),
            OscillatorType::Custom => self.generate_custom(buffer, freq_values),
        }
    }

    fn generate_sine(
        &mut self,
        type_: OscillatorType,
        buffer: &mut ChannelData,
        freq_values: &[f32],
    ) {
        for (o, &computed_freq) in buffer.iter_mut().zip(freq_values) {
            self.compute_params(type_, computed_freq);
            let idx = self.phase as usize;
            let inf_idx = idx % TABLE_LENGTH_USIZE;
            let sup_idx = (idx + 1) % TABLE_LENGTH_USIZE;

            // Linear interpolation
            *o = SINETABLE[inf_idx] * (1. - self.sine.interpol_ratio)
                + SINETABLE[sup_idx] * self.sine.interpol_ratio;

            // Optimized float modulo op
            self.phase = if self.phase + self.incr_phase >= TABLE_LENGTH_F32 {
                (self.phase + self.incr_phase) - TABLE_LENGTH_F32
            } else {
                self.phase + self.incr_phase
            };
        }
    }

    fn generate_sawtooth(
        &mut self,
        type_: OscillatorType,
        buffer: &mut ChannelData,
        freq_values: &[f32],
    ) {
        for (o, &computed_freq) in buffer.iter_mut().zip(freq_values) {
            self.compute_params(type_, computed_freq);
            let mut sample = (2.0 * self.phase) - 1.0;
            sample -= self.poly_blep(self.phase);

            // Optimized float modulo op
            self.phase += self.incr_phase;
            while self.phase >= 1. {
                self.phase -= 1.;
            }

            *o = sample;
        }
    }

    fn generate_square(
        &mut self,
        type_: OscillatorType,
        buffer: &mut ChannelData,
        freq_values: &[f32],
    ) {
        for (o, &computed_freq) in buffer.iter_mut().zip(freq_values) {
            self.compute_params(type_, computed_freq);
            let mut sample = if self.phase <= 0.5 { 1.0 } else { -1.0 };

            sample += self.poly_blep(self.phase);

            // Optimized float modulo op
            let mut shift_phase = self.phase + 0.5;
            while shift_phase >= 1. {
                shift_phase -= 1.
            }
            sample -= self.poly_blep(shift_phase);

            // Optimized float modulo op
            self.phase += self.incr_phase;
            while self.phase >= 1. {
                self.phase -= 1.
            }
            *o = sample;
        }
    }

    fn generate_triangle(
        &mut self,
        type_: OscillatorType,
        buffer: &mut ChannelData,
        freq_values: &[f32],
    ) {
        for (o, &computed_freq) in buffer.iter_mut().zip(freq_values) {
            self.compute_params(type_, computed_freq);
            let mut sample = if self.phase <= 0.5 { 1.0 } else { -1.0 };

            sample += self.poly_blep(self.phase);

            // Optimized float modulo op
            let mut shift_phase = self.phase + 0.5;
            while shift_phase >= 1. {
                shift_phase -= 1.
            }
            sample -= self.poly_blep(shift_phase);

            // Optimized float modulo op
            self.phase += self.incr_phase;
            while self.phase >= 1. {
                self.phase -= 1.
            }

            // Leaky integrator: y[n] = A * x[n] + (1 - A) * y[n-1]
            // Classic integration cannot be used due to float errors accumulation over execution time
            sample = self.incr_phase * sample + (1.0 - self.incr_phase) * self.triangle.last_output;
            self.triangle.last_output = sample;

            // Normalized amplitude into intervall [-1.0,1.0]
            *o = sample * 4.;
        }
    }

    fn generate_custom(&mut self, buffer: &mut ChannelData, freq_values: &[f32]) {
        for (o, &computed_freq) in buffer.iter_mut().zip(freq_values) {
            self.compute_periodic_params(computed_freq);
            if !self.periodic.disable_normalization {
                self.periodic.wavetable_state.phase = (self.periodic.wavetable_state.phase
                    + self.periodic.wavetable_state.incr_phase)
                    % self.periodic.wavetable_state.periodic_wavetable.len() as f32;
                *o = self.periodic.wavetable_state.periodic_wavetable
                    [self.periodic.wavetable_state.phase as usize];
            } else {
                let mut sample = 0.;
                for i in 1..self.periodic.phases.len() {
                    let gain = self.periodic.norms[i];
                    let phase = self.periodic.phases[i];
                    let incr_phase = self.periodic.incr_phases[i];
                    let interpol_ratio = self.periodic.interpol_ratios[i];
                    let idx = (phase + incr_phase) as usize;
                    let inf_idx = idx % TABLE_LENGTH_USIZE;
                    let sup_idx = (idx + 1) % TABLE_LENGTH_USIZE;

                    // Linear interpolation
                    sample += (SINETABLE[inf_idx] * (1. - interpol_ratio)
                        + SINETABLE[sup_idx] * interpol_ratio)
                        * gain
                        * self.periodic.norm_factor.unwrap_or(1.);

                    // Optimized float modulo op
                    self.periodic.phases[i] = if phase + incr_phase >= TABLE_LENGTH_F32 {
                        (phase + incr_phase) - TABLE_LENGTH_F32
                    } else {
                        phase + incr_phase
                    };
                }
                *o = sample;
            }
        }
    }

    fn generate_wavetable(
        phases: &mut [f32],
        incr_phases: &[f32],
        interpol_ratios: &[f32],
        norms: &[f32],
        buffer: &mut Vec<f32>,
    ) {
        buffer.clear();

        while phases[1] <= TABLE_LENGTH_F32 {
            let mut sample = 0.0;
            for i in 1..phases.len() {
                let gain = norms[i];
                let phase = phases[i];
                let incr_phase = incr_phases[i];
                let mu = interpol_ratios[i];
                let idx = (phase + incr_phase) as usize;
                let inf_idx = idx % TABLE_LENGTH_USIZE;
                let sup_idx = (idx + 1) % TABLE_LENGTH_USIZE;
                // Linear interpolation
                sample += (SINETABLE[inf_idx] * (1. - mu) + SINETABLE[sup_idx] * mu) * gain;
                phases[i] = phase + incr_phase;
            }

            buffer.push(sample);
        }
    }

    fn update_wavetable(&mut self) {
        self.periodic.wavetable_state.periodic_wavetable.clear();

        while self.periodic.phases[1] <= TABLE_LENGTH_F32 {
            let mut sample = 0.0;
            for i in 1..self.periodic.phases.len() {
                let gain = self.periodic.norms[i];
                let phase = self.periodic.phases[i];
                let incr_phase = self.periodic.incr_phases[i];
                let mu = self.periodic.interpol_ratios[i];
                let idx = (phase + incr_phase) as usize;
                let inf_idx = idx % TABLE_LENGTH_USIZE;
                let sup_idx = (idx + 1) % TABLE_LENGTH_USIZE;
                // Linear interpolation
                sample += (SINETABLE[inf_idx] * (1. - mu) + SINETABLE[sup_idx] * mu) * gain;
                self.periodic.phases[i] = phase + incr_phase;
            }

            self.periodic
                .wavetable_state
                .periodic_wavetable
                .push(sample);
        }
    }

    fn init_norm_factor(buffer: &[f32]) -> f32 {
        1. / buffer
            .iter()
            .copied()
            .reduce(f32::max)
            .expect("Maximum value not found")
    }

    fn update_norm_factor(&mut self) {
        self.periodic.norm_factor = Some(
            1. / self
                .periodic
                .wavetable_state
                .periodic_wavetable
                .iter()
                .copied()
                .reduce(f32::max)
                .expect("Maximum value not found"),
        );
    }

    fn poly_blep(&self, mut t: f32) -> f32 {
        let dt = self.incr_phase;
        if t < dt {
            t /= dt;
            t + t - t * t - 1.0
        } else if t > 1.0 - dt {
            t = (t - 1.0) / dt;
            t * t + t + t + 1.0
        } else {
            0.0
        }
    }
}

#[cfg(test)]

mod tests {
    use super::{PeriodicWave, PeriodicWaveOptions};
    use crate::context::AudioContext;

    #[test]
    #[should_panic]
    fn fails_to_build_when_real_is_too_short() {
        let context = AudioContext::new();

        let options = PeriodicWaveOptions {
            real: vec![0.],
            imag: vec![0., 0., 0.],
            disable_normalization: false,
        };

        let _periodic_wave = PeriodicWave::new(&context, Some(options));
    }

    #[test]
    #[should_panic]
    fn fails_to_build_when_imag_is_too_short() {
        let context = AudioContext::new();

        let options = PeriodicWaveOptions {
            real: vec![0., 0., 0.],
            imag: vec![0.],
            disable_normalization: false,
        };

        let _periodic_wave = PeriodicWave::new(&context, Some(options));
    }

    #[test]
    #[should_panic]
    fn fails_to_build_when_imag_and_real_not_equal_length() {
        let context = AudioContext::new();

        let options = PeriodicWaveOptions {
            real: vec![0., 0., 0.],
            imag: vec![0., 0.],
            disable_normalization: false,
        };

        let _periodic_wave = PeriodicWave::new(&context, Some(options));
    }
}
