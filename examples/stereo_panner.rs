use web_audio_api::context::{AudioContext, BaseAudioContext};
use web_audio_api::node::{AudioNode, AudioScheduledSourceNode};

fn main() {
    env_logger::init();
    let context = AudioContext::default();

    // pipe 2 oscillator into two panner, one on each side of the stereo image
    // inverse the direction of the panning every 4 second

    // create a stereo panner
    let panner_1 = context.create_stereo_panner();
    let mut pan_1 = -1.;
    panner_1.set_channel_count(1);
    panner_1.connect(&context.destination());
    panner_1.pan().set_value(pan_1);
    // create an oscillator
    let osc_1 = context.create_oscillator();
    osc_1.connect(&panner_1);
    osc_1.frequency().set_value(200.);
    osc_1.start();

    // create a stereo panner for mono input
    let panner_2 = context.create_stereo_panner();
    let mut pan_2 = 1.;
    panner_2.set_channel_count(1);
    panner_2.connect(&context.destination());
    panner_2.pan().set_value(pan_2);
    // create an oscillator
    let osc_2 = context.create_oscillator();
    osc_2.connect(&panner_2);
    osc_2.frequency().set_value(300.);
    osc_2.start();

    std::thread::sleep(std::time::Duration::from_secs(4));

    loop {
        // reverse the stereo image
        let now = context.current_time();

        panner_1.pan().set_value_at_time(pan_1, now);
        pan_1 = if pan_1 == 1. { -1. } else { 1. };
        panner_1.pan().linear_ramp_to_value_at_time(pan_1, now + 1.);

        panner_2.pan().set_value_at_time(pan_2, now);
        pan_2 = if pan_2 == 1. { -1. } else { 1. };
        panner_2.pan().linear_ramp_to_value_at_time(pan_2, now + 1.);

        std::thread::sleep(std::time::Duration::from_secs(4));
    }
}
