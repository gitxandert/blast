use alsa_sys::*;
use libc::{self, EAGAIN, EPIPE, TCSANOW, c_int, cfmakeraw, tcgetattr, tcsetattr, termios};
use std::{
    collections::HashMap,
    ffi::CString,
    io::{Read, Write},
    ptr,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};

use crate::audio_processing::{
    blast_time::sample_rate,
    commands::{CmdProcessor, CmdQueue, EngineState},
    engine::Conductor,
    line_editor::{Key, LineEditor, parse_key},
};
use crate::file_parsing::decode_helpers::AudioFile;

pub fn run_blast(tracks: HashMap<String, AudioFile>, sample_rate: u32, num_channels: u32) {
    // initialize audio engine and engine state
    let tracks_for_state = tracks.clone().into_values().collect();
    let engine_state = EngineState::new(tracks_for_state, num_channels as usize);
    let mut conductor = Conductor::prepare(num_channels as usize, tracks);

    sample_rate::set(sample_rate);

    // take over STDIN
    let marker = Arc::new(Mutex::new(0usize));
    let editor = Arc::new(Mutex::new(LineEditor::new()));
    let repl_chars = ['^', 'X', 'v', '>', 'X', '<', 'Z'];

    {
        let marker = marker.clone();
        let marker_for_mt = marker.clone();
        let editor = editor.clone();

        thread::spawn(move || {
            loop {
                {
                    let mut m = marker_for_mt.lock().unwrap();
                    *m = (*m + 1) % repl_chars.len();
                }
                thread::sleep(Duration::from_millis(100));
            }
        });
        thread::spawn(move || {
            loop {
                {
                    // redraw marker + input_text
                    let m = *marker.lock().unwrap();
                    print!("{}", editor.lock().unwrap().render(repl_chars[m]));

                    /*
                                      let mut width = 80usize;
                                      let mut height = 40usize;
                                      let mut divider = width / 3;

                                      unsafe {
                                          let mut ws: winsize = mem::zeroed();
                                          if ioctl(STDOUT_FILENO, TIOCGWINSZ.into(), &mut ws) == 0 {
                                              width = ws.ws_col as usize;
                                              height = ws.ws_row as usize;
                                              divider = width / 3;
                                          };
                                      }

                                      let mut term = String::with_capacity(width * height);

                                      for j in {0..height} {
                                          for i in {0..width} {
                                              if i == divider {
                                                  term.push('|');
                                              } else {
                                                  term.push(' ');
                                              }
                                          }
                                      }
                                      print!("\x1b[H{}", term);

                                      print!("\x1b[H");
                    */
                    std::io::stdout().flush().unwrap();
                }
                thread::sleep(Duration::from_millis(15));
            }
        });
    }

    raw_mode("on");

    // create command queue between command and audio threads
    // and intialize the command processor with engine state
    // (just tracks for now)
    let queue = Arc::new(CmdQueue::new(256));
    let mut cmd_processor = CmdProcessor::new(engine_state);
    // REPL
    println!("");
    {
        let editor = editor.clone();
        let queue = queue.clone();

        let mut cmd_history = Vec::<String>::new();
        let mut cmd_idx = cmd_history.len();

        thread::spawn(move || {
            let mut input = std::iter::from_fn(|| Some(read_char()));
            loop {
                let key = parse_key(&mut input);

                match key {
                    Key::Enter => {
                        // enter
                        print!("\n");
                        let mut buf = editor.lock().unwrap();

                        let cmd = buf.take();
                        cmd_history.push(cmd.clone());
                        cmd_idx = cmd_history.len();

                        // checks the queue before parsing, so a full
                        // queue can't desync the mirror from the engine
                        match cmd_processor.submit(&cmd, &queue) {
                            Ok(Some(text)) => println!("\n{text}"),
                            Ok(None) => (),
                            Err(error) => {
                                buf.clear();
                                println!("\nErr: {error}");
                            }
                        }

                        buf.clear();
                    }
                    Key::Backspace => {
                        editor.lock().unwrap().backspace();
                    }
                    Key::Interrupt => {
                        // CTL + C
                        raw_mode("off");
                        editor.lock().unwrap().clear();
                        println!("\nInterrupted.");
                        std::process::exit(130);
                    }
                    Key::Delete => editor.lock().unwrap().delete(),
                    Key::Home => editor.lock().unwrap().home(),
                    Key::End => editor.lock().unwrap().end(),
                    Key::Left => editor.lock().unwrap().left(),
                    Key::Right => editor.lock().unwrap().right(),
                    Key::Up => {
                        if cmd_idx > 0 {
                            cmd_idx -= 1;
                            let prev = cmd_history.get(cmd_idx).cloned().unwrap_or_default();
                            editor.lock().unwrap().set(prev);
                        }
                    }
                    Key::Down => {
                        if cmd_idx < cmd_history.len() {
                            cmd_idx += 1;
                            // past the newest entry -> empty line
                            let next = cmd_history.get(cmd_idx).cloned().unwrap_or_default();
                            editor.lock().unwrap().set(next);
                        }
                    }
                    Key::Char(c) => {
                        editor.lock().unwrap().insert(c);
                    }
                    Key::Ignore => (),
                }
            }
        });
    }

    // install signal catchers and panic callbacks
    // to break main loop and turn off raw_mode
    install_sigterm_handler();
    install_panic_hook();

    // audio setup and main loop
    unsafe {
        // open pcm
        let mut handle: *mut snd_pcm_t = ptr::null_mut();
        let dev = CString::new("hw:0,0").unwrap();

        check_code(
            snd_pcm_open(&mut handle, dev.as_ptr(), SND_PCM_STREAM_PLAYBACK, 0),
            "snd_pcm_open",
        );

        // config hardware
        let mut hw: *mut snd_pcm_hw_params_t = ptr::null_mut();
        snd_pcm_hw_params_malloc(&mut hw);
        snd_pcm_hw_params_any(handle, hw);

        check_code(
            snd_pcm_hw_params_set_access(handle, hw, SND_PCM_ACCESS_MMAP_INTERLEAVED),
            "set_access",
        );
        check_code(
            snd_pcm_hw_params_set_format(handle, hw, SND_PCM_FORMAT_S16_LE),
            "set_format",
        );
        check_code(
            snd_pcm_hw_params_set_channels(handle, hw, num_channels),
            "set_ channels",
        );
        check_code(
            snd_pcm_hw_params_set_rate(handle, hw, sample_rate, 0),
            "set_rate",
        );

        let mut period_size: snd_pcm_uframes_t = 128;
        check_code(
            snd_pcm_hw_params_set_period_size_near(handle, hw, &mut period_size, 0 as *mut i32),
            "set_period_size",
        );

        let mut buffer_size: snd_pcm_uframes_t = period_size * 4;
        check_code(
            snd_pcm_hw_params_set_buffer_size_near(handle, hw, &mut buffer_size),
            "set_buffer_size",
        );

        check_code(snd_pcm_hw_params(handle, hw), "snd_pcm_hw_params");
        snd_pcm_hw_params_free(hw);

        // config software params
        let mut sw: *mut snd_pcm_sw_params_t = ptr::null_mut();
        snd_pcm_sw_params_malloc(&mut sw);
        snd_pcm_sw_params_current(handle, sw);

        let mut boundary: snd_pcm_uframes_t = 0;
        snd_pcm_sw_params_get_boundary(sw, &mut boundary);
        snd_pcm_sw_params_set_stop_threshold(handle, sw, boundary);
        // start immediately upon write
        check_code(
            snd_pcm_sw_params_set_start_threshold(handle, sw, period_size),
            "set_start_threshold",
        );

        // wake when period is available
        check_code(
            snd_pcm_sw_params_set_avail_min(handle, sw, period_size),
            "set_avail_min",
        );

        check_code(snd_pcm_sw_params(handle, sw), "snd_pcm_sw_params");
        snd_pcm_sw_params_free(sw);

        // prepare device
        check_code(snd_pcm_prepare(handle), "snd_pcm_prepare");

        loop {
            if TERM_RECEIVED.load(Ordering::Relaxed) {
                break;
            }

            // apply commands from queue
            while let Some(cmd) = queue.try_pop() {
                conductor.apply(cmd);
            }

            let avail = snd_pcm_avail_update(handle) as i32;
            if avail == -EPIPE {
                // underrun
                snd_pcm_recover(handle, avail, 1);
                continue;
            }
            if avail < 0 {
                snd_pcm_recover(handle, avail, 1);
                continue;
            }
            if avail < period_size as i32 {
                let r = snd_pcm_wait(handle, -1);
                if r < 0 {
                    snd_pcm_recover(handle, r, 1);
                }
                continue;
            }

            // get remaining frames to write
            let mut remaining = avail as snd_pcm_uframes_t;

            while remaining > 0 {
                let mut areas_ptr: *const snd_pcm_channel_area_t = ptr::null();
                let mut offset: snd_pcm_uframes_t = 0;
                let mut frames: snd_pcm_uframes_t = remaining;

                // mmap begin
                let r = snd_pcm_mmap_begin(handle, &mut areas_ptr, &mut offset, &mut frames);
                if r == -EAGAIN {
                    break; // hardware not ready
                }
                if r < 0 {
                    snd_pcm_recover(handle, r, 1);
                    break;
                }

                // write to DMA buffer
                conductor.coordinate(areas_ptr, offset, frames);

                let committed = snd_pcm_mmap_commit(handle, offset, frames) as i32;
                if committed < 0 {
                    snd_pcm_recover(handle, committed, 1);
                    break;
                }

                remaining -= committed as snd_pcm_uframes_t;
            }
            if snd_pcm_state(handle) != SND_PCM_STATE_RUNNING {
                snd_pcm_start(handle);
            }
        }
    }

    editor.lock().unwrap().clear();
    raw_mode("off");
}

// check error codes for alsa
//
unsafe fn check_code(code: c_int, ctx: &str) {
    if code < 0 {
        let msg = unsafe { std::ffi::CStr::from_ptr(snd_strerror(code)) };
        panic!("{ctx}: {}", msg.to_string_lossy());
    }
}

// signal and panic handlers
//
static TERM_RECEIVED: AtomicBool = AtomicBool::new(false);

extern "C" fn handle_sigterm(_sig: libc::c_int) {
    TERM_RECEIVED.store(true, Ordering::Relaxed);
    raw_mode("off");
}

fn install_sigterm_handler() {
    unsafe {
        let mut sa: libc::sigaction = std::mem::zeroed();
        sa.sa_sigaction = handle_sigterm as *const () as usize;
        sa.sa_flags = 0;

        // non-blocking
        libc::sigemptyset(&mut sa.sa_mask);

        // register
        libc::sigaction(libc::SIGTERM, &sa, std::ptr::null_mut());
    }
}

fn install_panic_hook() {
    std::panic::set_hook(Box::new(|info| {
        raw_mode("off");
        eprintln!("\nPanic: {info}");
        std::process::exit(130);
    }));
}

// terminal takeover funcs
//
static mut ORIG_TERM: Option<termios> = None;

fn raw_mode(switch: &str) {
    unsafe {
        let fd = libc::STDIN_FILENO;

        let mut term: termios = std::mem::zeroed();
        tcgetattr(fd, &mut term);

        match switch {
            "on" => {
                ORIG_TERM = Some(term);
                let mut raw = term;
                cfmakeraw(&mut raw);
                tcsetattr(fd, TCSANOW, &raw);
            }
            _ => {
                if let Some(orig) = ORIG_TERM {
                    tcsetattr(fd, TCSANOW, &orig);
                }
            }
        };
    }
}

fn read_char() -> u8 {
    let mut buf = [0u8; 1];
    std::io::stdin().read_exact(&mut buf).unwrap();
    buf[0]
}
