use std::io::BufRead;
use web_audio_api::context::{AudioContext, AudioContextOptions, BaseAudioContext};
use web_audio_api::node::{
    AudioNode, AudioScheduledSourceNode, PannerNode, PannerOptions, PanningModelType,
};

fn main() {
    let ctx_opts = AudioContextOptions {
        sample_rate: Some(44100.),
        ..AudioContextOptions::default()
    };
    let context = AudioContext::new(ctx_opts);

    // Move listener slightly out of carthesian center to prevent numerical artefacts
    context.listener().position_x().set_value(0.01);
    context.listener().position_y().set_value(0.01);
    context.listener().position_z().set_value(0.01);

    // Create looping 'siren' sound
    let file = std::fs::File::open("samples/siren.mp3").unwrap();
    let buffer = context.decode_audio_data_sync(file).unwrap();
    let tone = context.create_buffer_source();
    tone.set_buffer(buffer);
    tone.set_loop(true);
    tone.start();

    // Set up a panner with HRTF panning
    let opts = PannerOptions {
        panning_model: PanningModelType::HRTF,
        ..PannerOptions::default()
    };

    let panner = PannerNode::new(&context, opts);
    tone.connect(&panner);
    panner.connect(&context.destination());

    // Panner will move in circles around the listener, in the horizontal plane (x-z).
    // A frequency of 0.25 means the circle takes 4 seconds to complete.
    // Amplify with a value of 10. so the radius of the circle is 10. This means the distance gain
    // will be audible (ref distance = 1.)
    //
    // Make x-value a periodic wave
    let moving = context.create_oscillator();
    moving.frequency().set_value_at_time(0.25, 0.);
    let gain = context.create_gain();
    gain.gain().set_value_at_time(10., 0.);
    moving.connect(&gain);
    gain.connect(panner.position_x());
    moving.start();

    // Make y-value a periodic wave, delayed so it forms a circle with the x-value
    let moving = context.create_oscillator();
    moving.frequency().set_value_at_time(0.25, 0.);
    let delay = context.create_delay(1.);
    delay
        .delay_time()
        .set_value_at_time(std::f32::consts::PI / 4., 0.);
    let gain = context.create_gain();
    gain.gain().set_value_at_time(10., 0.);
    moving.connect(&delay);
    delay.connect(&gain);
    gain.connect(panner.position_z());
    moving.start();

    // enjoy listening
    println!("Siren is circling in the horizontal plane around the listener");
    println!("HRTF enabled, press <Enter> to toggle");

    let mut hrtf = true;
    std::io::stdin().lock().lines().for_each(|_| {
        hrtf = !hrtf;
        let p = if hrtf {
            PanningModelType::HRTF
        } else {
            PanningModelType::EqualPower
        };
        panner.set_panning_model(p);
        println!("PanningMode: {:?}", panner.panning_model());
    });
}
