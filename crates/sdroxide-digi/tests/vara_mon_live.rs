//! VARA-MON end to end, against a real `xara-monitor`.
//!
//! Opt-in, because the monitor is the operator's program and not part of this
//! tree: set `XARA_MONITOR` to the executable and `VARA_WAV` to a recording of
//! a VARA HF 2300 session (mono, any rate), then
//!
//! ```text
//! XARA_MONITOR=/path/xara-monitor VARA_WAV=/path/session.wav \
//!     cargo test -p sdroxide-digi --test vara_mon_live -- --ignored --nocapture
//! ```
//!
//! The recording is fed through the controller in real-time-sized blocks at the
//! file's own rate and at 12 kHz (so the resampler is exercised too), and the
//! test passes once the monitor has reported a whole link.

use std::time::{Duration, Instant, SystemTime};

use sdroxide_digi::{DigiAction, DigiEngine, VaraMonController};
use sdroxide_types::{DigiConfig, VaraMonConfig, VaraMonKind, VaraMonState};

fn read_wav(path: &str) -> (Vec<f32>, f64) {
    let mut r = hound::WavReader::open(path).expect("open VARA_WAV");
    let spec = r.spec();
    assert_eq!(spec.channels, 1, "mono, please");
    let samples: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Int => {
            let full = (1i64 << (spec.bits_per_sample - 1)) as f32;
            r.samples::<i32>().map(|s| s.unwrap() as f32 / full).collect()
        }
        hound::SampleFormat::Float => r.samples::<f32>().map(Result::unwrap).collect(),
    };
    (samples, f64::from(spec.sample_rate))
}

/// Crude decimation by an integer factor with a boxcar — enough for a 3 kHz
/// signal going from 48 to 12 kHz in a test.
fn decimate(x: &[f32], k: usize) -> Vec<f32> {
    x.chunks(k).map(|c| c.iter().sum::<f32>() / c.len() as f32).collect()
}

fn run(samples: &[f32], rate: f64) {
    let exe = std::env::var("XARA_MONITOR").expect("XARA_MONITOR");
    let vm = VaraMonConfig { exe, calls: "EA5XYZ".into(), ..Default::default() };
    let cfg = DigiConfig { my_call: "EA5ABC".into(), ..Default::default() };
    let mut c = VaraMonController::new(cfg, vm, rate);

    let block = (rate / 50.0) as usize; // 20 ms, like the engine's tap
    let start = Instant::now();
    let mut last = None;
    for chunk in samples.chunks(block) {
        c.on_rx_audio(chunk);
        for a in c.poll(SystemTime::now(), 14_105_000.0) {
            if let DigiAction::Status(s) = a {
                last = s.vara_mon;
            }
        }
        // Faster than real time, but not so fast that a 5 s queue overflows.
        std::thread::sleep(Duration::from_micros(500));
    }
    // Silence after the recording, so the monitor's look-behind flushes.
    let quiet = vec![0.0f32; block];
    let deadline = Instant::now() + Duration::from_secs(60);
    while Instant::now() < deadline {
        c.on_rx_audio(&quiet);
        for a in c.poll(SystemTime::now(), 14_105_000.0) {
            if let DigiAction::Status(s) = a {
                last = s.vara_mon;
            }
        }
        if last.as_ref().is_some_and(|s| s.links > 0) {
            break;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    let st = last.expect("a status");
    eprintln!(
        "rate {rate}: {:.1} s, state {:?} v{} {}",
        start.elapsed().as_secs_f32(),
        st.state,
        st.version,
        st.message
    );
    for l in &st.lines {
        eprintln!("  {:?} {}", l.kind, l.text);
    }
    for s in &st.stations {
        eprintln!(
            "  heard {} peer {} frames {} best {:?} L{}",
            s.call, s.peer, s.frames, s.best_snr_db, s.top_level
        );
    }
    assert_eq!(st.state, VaraMonState::Running, "{}", st.message);
    assert_eq!(st.dropped_blocks, 0, "the monitor fell behind");
    assert!(st.links >= 1, "no complete link reported");
    assert!(st.lines.iter().any(|l| l.kind == VaraMonKind::Header));
    assert!(st.stations.iter().any(|s| s.call == "EA5ABC" && s.peer == "EA5XYZ"));
}

#[test]
#[ignore = "needs XARA_MONITOR and VARA_WAV"]
fn a_recorded_session_is_reported_at_its_own_rate() {
    let (x, rate) = read_wav(&std::env::var("VARA_WAV").expect("VARA_WAV"));
    run(&x, rate);
}

#[test]
#[ignore = "needs XARA_MONITOR and VARA_WAV"]
fn a_recorded_session_is_reported_through_the_resampler() {
    let (x, rate) = read_wav(&std::env::var("VARA_WAV").expect("VARA_WAV"));
    assert_eq!(rate, 48_000.0, "this one wants a 48 kHz recording");
    run(&decimate(&x, 4), 12_000.0);
}
