mod compositor;
mod logging_backend;
mod openvr;
mod shutdown_signal;
mod statistics;
mod video_encoder;

use bridgevr_common::{
    audio::*, constants::*, data::*, rendering::*, ring_channel::*, sockets::*, *,
};
use compositor::*;
use lazy_static::lazy_static;
use log::*;
use openvr::*;
use shutdown_signal::ShutdownSignal;
use statistics::*;
use std::{
    ffi::*,
    os::raw::*,
    ptr::null_mut,
    sync::{mpsc::*, *},
    thread,
    time::*,
};
use video_encoder::*;

// BridgeVR uses parking_lot's mutex because it unlocks itself in case of a thread that holds the
// lock panics. This reduces the chance of SteamVR noticing the crash and displaying "headset not
// found" error.
use parking_lot::Mutex;

const TRACE_CONTEXT: &str = "Driver main";

const TIMEOUT: Duration = Duration::from_secs(1);

fn get_settings() -> StrResult<Settings> {
    load_settings(env!("SETTINGS_PATH"))
}

fn begin_server_loop(
    graphics: Arc<GraphicsContext>,
    openvr_backend: Arc<Mutex<OpenvrBackend>>,
    shutdown_signal_sender: Sender<ShutdownSignal>,
    shutdown_signal_receiver: Receiver<ShutdownSignal>,
    session_desc_loader: Arc<Mutex<SessionDescLoader>>,
) -> StrResult {
    let timeout = get_settings()
        .map(|s| Duration::from_secs(s.openvr.timeout_seconds))
        .unwrap_or(TIMEOUT);
    let mut deadline = Instant::now() + timeout;

    let try_connect = {
        let openvr_backend = openvr_backend.clone();
        move |shutdown_signal_receiver: &Receiver<ShutdownSignal>| -> StrResult<ShutdownSignal> {
            let settings = if let Ok(settings) = get_settings() {
                settings
            } else {
                thread::sleep(Duration::from_secs(1));
                get_settings()?
            };
            let receiver_data_port = settings.connection.starting_data_port;
            let mut next_sender_data_port = settings.connection.starting_data_port;

            let (client_handshake_packet, client_candidate_desc) =
                search_client(&settings.connection.client_ip, TIMEOUT)?;

            if client_handshake_packet.version < BVR_MIN_VERSION_CLIENT {
                return trace_str!(
                    "Espected client of version {} or greater, found {}.",
                    BVR_MIN_VERSION_CLIENT,
                    client_handshake_packet.version
                );
            }

            session_desc_loader
                .lock()
                .get_mut()
                .last_client_handshake_packet = Some(client_handshake_packet.clone());
            session_desc_loader
                .lock()
                .save()
                .map_err(|e| warn!("{}", e))
                .ok();

            let target_eye_resolution = match &settings.video.frame_size {
                FrameSize::Scale(scale) => {
                    let (native_eye_width, native_eye_height) =
                        client_handshake_packet.native_eye_resolution;
                    let width = (native_eye_width as f32 * *scale) as _;
                    let height = (native_eye_height as f32 * *scale) as _;
                    (width, height)
                }
                FrameSize::Absolute(width, height) => (*width, *height),
            };

            let server_handshake_packet = ServerHandshakePacket {
                version: BVR_VERSION_SERVER,
                settings: settings.clone(),
                target_eye_resolution,
            };

            let client_statistics = Arc::new(Mutex::new(ClientStatistics::default()));

            let connection_manager = Arc::new(Mutex::new(ConnectionManager::connect_to_client(
                client_candidate_desc,
                server_handshake_packet,
                {
                    let shutdown_signal_sender = shutdown_signal_sender.clone();
                    let openvr_backend = openvr_backend.clone();
                    move |message| match message {
                        ClientMessage::Update(input) => openvr_backend.lock().update_input(&input),
                        ClientMessage::Statistics(client_stats) => {
                            *client_statistics.lock() = client_stats
                        }
                        ClientMessage::Disconnected => {
                            shutdown_signal_sender
                                .send(ShutdownSignal::ClientDisconnected)
                                .ok();
                        }
                    }
                },
            )?));

            let mut slice_producers = vec![];
            let mut slice_consumers = vec![];
            for _ in 0..settings.video.frame_slice_count {
                let (producer, consumer) = queue_channel_split();
                slice_producers.push(producer);
                slice_consumers.push(consumer);
            }

            let (present_producer, present_consumer) = queue_channel_split();

            let mut compositor = Compositor::new(
                graphics.clone(),
                CompositorDesc {
                    target_eye_resolution,
                    filter_type: settings.video.composition_filtering,
                    ffr_desc: settings.video.foveated_rendering.clone().into_option(),
                },
                present_consumer,
                slice_producers,
            )?;

            let video_encoder_resolution = compositor.encoder_resolution();

            let mut video_encoders = vec![];
            for (idx, slice_consumer) in slice_consumers.into_iter().enumerate() {
                let (video_packet_producer, video_packet_consumer) = queue_channel_split();

                video_encoders.push(VideoEncoder::new(
                    &format!("Video encoder loop {}", idx),
                    settings.video.encoder.clone(),
                    video_encoder_resolution,
                    client_handshake_packet.fps,
                    slice_consumer,
                    video_packet_producer,
                )?);

                connection_manager.lock().begin_send_buffers(
                    &format!("Video packet sender loop {}", idx),
                    next_sender_data_port,
                    video_packet_consumer,
                )?;
                next_sender_data_port += 1;
            }

            let mut maybe_game_audio_recorder = match &settings.game_audio {
                Switch::Enabled(desc) => {
                    let (producer, consumer) = queue_channel_split();
                    let game_audio_recorder =
                        AudioRecorder::start_recording(desc.input_device_index, true, producer)?;
                    connection_manager.lock().begin_send_buffers(
                        "Game audio send loop",
                        next_sender_data_port,
                        consumer,
                    )?;
                    Some(game_audio_recorder)
                }
                Switch::Disabled => None,
            };

            let mut maybe_microphone_player = match &settings.microphone {
                Switch::Enabled(desc) => {
                    let (producer, consumer) = keyed_channel_split(Duration::from_millis(100));
                    connection_manager.lock().begin_receive_indexed_buffers(
                        "Microphone audio receive loop",
                        receiver_data_port,
                        producer,
                    )?;
                    Some(AudioPlayer::start_playback(
                        desc.output_device_index,
                        consumer,
                    )?)
                }
                Switch::Disabled => None,
            };

            openvr_backend
                .lock()
                .initialize_for_client_or_request_restart(
                    &settings,
                    session_desc_loader.lock().get_mut(),
                    present_producer,
                    connection_manager.clone(),
                )?;

            let statistics_interval = Duration::from_secs(1);
            let res = loop {
                log_statistics();

                match shutdown_signal_receiver.recv_timeout(statistics_interval) {
                    Ok(signal) => break Ok(signal),
                    Err(RecvTimeoutError::Disconnected) => {
                        break Ok(ShutdownSignal::BackendShutdown)
                    }
                    _ => (),
                }
            };

            if let Ok(ShutdownSignal::BackendShutdown) = res {
                connection_manager
                    .lock()
                    .send_message_tcp(&ServerMessage::Shutdown)
                    .map_err(|e| debug!("{}", e))
                    .ok();
            }

            // Dropping an object that contains a thread loop requires waiting for some actions to
            // timeout. The drops happen sequentially so the time required to execute them is at
            // worst the sum of all timeouts. By calling request_stop() on all objects involved I
            // can buffer all the shutdown requests at once, so if we drop the objects immediately
            // after, the time needed for all drops is at worst the maximum of all the timeouts.

            connection_manager.lock().request_stop();
            compositor.request_stop();

            for video_encoder in &mut video_encoders {
                video_encoder.request_stop();
            }

            if let Some(recorder) = &mut maybe_game_audio_recorder {
                recorder.request_stop();
            }

            if let Some(player) = &mut maybe_microphone_player {
                player.request_stop();
            }

            res
        }
    };

    trace_err!(thread::Builder::new()
        .name("Connection/statistics loop".into())
        .spawn(move || {
            while Instant::now() < deadline {
                match show_err!(try_connect(&shutdown_signal_receiver)) {
                    Ok(ShutdownSignal::ClientDisconnected) => deadline = Instant::now() + timeout,
                    Ok(ShutdownSignal::BackendShutdown) => break,
                    Err(()) => {
                        if let Ok(ShutdownSignal::BackendShutdown)
                        | Err(TryRecvError::Disconnected) = shutdown_signal_receiver.try_recv()
                        {
                            break;
                        }
                    }
                }
                openvr_backend.lock().deinitialize_for_client();
            }
        })
        .map(|_| ()))
}

// To make a minimum system, BridgeVR needs to instantiate OpenvrServer.
// This means that most OpenVR related settings cannot be changed while the driver is running.
// OpenvrServer needs to be instantiated statically because if it get destroyed SteamVR will find
// invalid pointers.
// Avoid crashing or returning errors, otherwise SteamVR would complain that there is no HMD.
// If get_settings() returns an error, create the OpenVR server anyway, even if it remains in an
// unusable state.

struct EmptySystem {
    graphics: Arc<GraphicsContext>,
    openvr_backend: Arc<Mutex<OpenvrBackend>>,
    shutdown_signal_sender: Arc<Mutex<Sender<ShutdownSignal>>>,
    shutdown_signal_receiver_temp: Arc<Mutex<Option<Receiver<ShutdownSignal>>>>,
    session_desc_loader: Arc<Mutex<SessionDescLoader>>,
}

fn create_empty_system() -> StrResult<EmptySystem> {
    let maybe_settings = get_settings()
        .map_err(|_| error!("Cannot read settings. BridgeVR server will be in an invalid state."))
        .ok();

    let session_desc_loader = Arc::new(Mutex::new(SessionDescLoader::load(env!("SESSION_PATH"))));

    let graphics = Arc::new(GraphicsContext::new(None)?);

    let (shutdown_signal_sender, shutdown_signal_receiver) = mpsc::channel();

    let openvr_backend = Arc::new(Mutex::new(OpenvrBackend::new(
        graphics.clone(),
        maybe_settings.as_ref(),
        &session_desc_loader.lock().get_mut(),
        shutdown_signal_sender.clone(),
    )));

    Ok(EmptySystem {
        graphics,
        openvr_backend,
        shutdown_signal_sender: Arc::new(Mutex::new(shutdown_signal_sender)),
        shutdown_signal_receiver_temp: Arc::new(Mutex::new(Some(shutdown_signal_receiver))),
        session_desc_loader,
    })
}

// OpenVR entry point
#[no_mangle]
#[allow(clippy::missing_safety_doc)]
pub unsafe extern "C" fn HmdDriverFactory(
    interface_name: *const c_char,
    return_code_ptr: *mut c_int,
) -> *mut c_void {
    use openvr_driver_sys as vr;
    logging_backend::init_logging();

    lazy_static! {
        static ref MAYBE_EMPTY_SYSTEM: StrResult<EmptySystem> = create_empty_system();
    }

    let try_create_server = || -> StrResult<_> {
        let sys = (*MAYBE_EMPTY_SYSTEM).as_ref()?;
        begin_server_loop(
            sys.graphics.clone(),
            sys.openvr_backend.clone(),
            sys.shutdown_signal_sender.lock().clone(),
            // this unwrap is safe because `shutdown_signal_receiver_temp` has just been set
            sys.shutdown_signal_receiver_temp.lock().take().unwrap(),
            sys.session_desc_loader.clone(),
        )?;

        Ok(sys.openvr_backend.lock().server_ptr())
    };

    match try_create_server() {
        Ok(mut server_ptr) => {
            if CStr::from_ptr(interface_name)
                == CStr::from_bytes_with_nul_unchecked(vr::IServerTrackedDeviceProvider_Version)
            {
                server_ptr = null_mut();
            }

            if server_ptr.is_null() && !return_code_ptr.is_null() {
                *return_code_ptr = vr::VRInitError_Init_InterfaceNotFound as _;
            }

            server_ptr as _
        }
        Err(e) => {
            show_err_str!("{}", e);
            null_mut()
        }
    }
}
