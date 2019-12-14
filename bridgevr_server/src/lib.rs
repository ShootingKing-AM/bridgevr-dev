mod compositor;
mod logging_backend;
mod openvr_backend;
mod shutdown_signal;
mod statistics;
mod video_encoder;

use bridgevr_common::{audio::*, constants::*, data::*, ring_channel::*, sockets::*, *};
use compositor::Compositor;
use lazy_static::lazy_static;
use log::*;
use openvr_backend::*;
use openvr_driver::*;
use shutdown_signal::ShutdownSignal;
use statistics::*;
use std::{
    ffi::*,
    sync::{mpsc::*, *},
    thread,
    time::*,
};
use video_encoder::*;

// BridgeVR uses parking_lot's mutex because it unlocks itself in case of a thread that holds the
// lock panics. This reduces the chance of SteamVR noticing the crash and displaying "headset not
// found" error.
use parking_lot::Mutex;

const TIMEOUT: Duration = Duration::from_secs(1);

fn get_settings() -> StrResult<Settings> {
    load_settings(env!("SETTINGS_PATH"))
}

type ShutdownSignalChannel = (Sender<ShutdownSignal>, Receiver<ShutdownSignal>);

fn begin_server_loop(
    compositor: Arc<Mutex<Compositor>>,
    openvr_backend: Arc<Mutex<OpenvrBackend>>,
    (shutdown_signal_sender, shutdown_signal_receiver): ShutdownSignalChannel,
    session_desc_loader: Arc<Mutex<SessionDescLoader>>,
) -> StrResult<()> {
    let timeout = Duration::from_secs(
        get_settings()
            .map(|s| s.openvr.timeout_seconds)
            .unwrap_or(1),
    );
    let mut deadline = Instant::now() + timeout;

    let try_connect = {
        let compositor = compositor.clone();
        let openvr_backend = openvr_backend.clone();
        let shutdown_signal_sender = shutdown_signal_sender.clone();

        // if any error is encountered, display it immediately to avoid waiting for every object to
        // drop
        move |shutdown_signal_receiver: &Receiver<ShutdownSignal>| -> Result<ShutdownSignal, ()> {
            let settings = if let Ok(settings) = get_settings() {
                settings
            } else {
                thread::sleep(Duration::from_secs(1));
                display_err!(get_settings())?
            };
            let receiver_data_port = settings.connection.starting_data_port;
            let mut next_sender_data_port = settings.connection.starting_data_port;

            let (client_handshake_packet, client_candidate_desc) =
                display_err!(search_client(&settings.connection.client_ip, TIMEOUT))?;

            if client_handshake_packet.version < BVR_MIN_VERSION_CLIENT {
                display_err_str!(
                    "Espected client of version {} or greater, found {}.",
                    BVR_MIN_VERSION_CLIENT,
                    client_handshake_packet.version
                );
                return Err(());
            }

            {
                let mut session_desc_loader = session_desc_loader.lock();
                session_desc_loader.get_mut().last_client_handshake_packet =
                    Some(client_handshake_packet.clone());
                session_desc_loader.save().map_err(|e| warn!("{}", e)).ok();
            }

            let (target_eye_width, target_eye_height) = match &settings.video.frame_size {
                FrameSize::Scale(scale) => {
                    let (native_eye_width, native_eye_height) =
                        client_handshake_packet.native_eye_resolution;
                    let width = (native_eye_width as f32 * *scale) as _;
                    let height = (native_eye_height as f32 * *scale) as _;
                    (width, height)
                }
                FrameSize::Absolute { width, height } => (*width, *height),
            };

            let server_handshake_packet = ServerHandshakePacket {
                version: BVR_VERSION_SERVER,
                settings: settings.clone(),
                target_eye_width,
                target_eye_height,
            };

            let client_statistics = Arc::new(Mutex::new(ClientStatistics::default()));

            let connection_manager = Arc::new(Mutex::new(display_err!(
                ConnectionManager::connect_to_client(
                    client_candidate_desc,
                    server_handshake_packet,
                    {
                        let shutdown_signal_sender = shutdown_signal_sender.clone();
                        let client_statistics = client_statistics.clone();
                        let openvr_backend = openvr_backend.clone();
                        move |message| match message {
                            ClientMessage::Update(input) => {
                                openvr_backend.lock().update_input(&input)
                            }
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
                )
            )?));

            let mut slice_producers = vec![];
            let mut slice_consumers = vec![];
            for _ in 0..settings.video.slice_count {
                let (producer, consumer) = queue_channel_split();
                slice_producers.push(producer);
                slice_consumers.push(consumer);
            }

            let mut video_encoders = vec![];
            for slice_consumer in slice_consumers {
                let (video_packet_producer, video_packet_consumer) = queue_channel_split();

                video_encoders.push(display_err!(VideoEncoder::new(
                    settings.video.encoder.clone(),
                    slice_consumer,
                    video_packet_producer,
                ))?);

                display_err!(connection_manager
                    .lock()
                    .begin_send_buffers(next_sender_data_port, video_packet_consumer))?;
                next_sender_data_port += 1;
            }

            let mut maybe_game_audio_recorder = match settings.audio.loopback_device_index {
                Switch::Enabled(device_idx) => {
                    let (producer, consumer) = queue_channel_split();
                    let audio_recorder =
                        display_err!(AudioRecorder::start_recording(device_idx, true, producer))?;
                    display_err!(connection_manager
                        .lock()
                        .begin_send_buffers(next_sender_data_port, consumer))?;
                    Some(audio_recorder)
                }
                Switch::Disabled => None,
            };

            let mut maybe_microphone_player = match &settings.audio.microphone {
                Switch::Enabled(mic) => {
                    let (producer, consumer) = keyed_channel_split(Duration::from_millis(100));
                    display_err!(connection_manager
                        .lock()
                        .begin_receive_indexed_buffers(receiver_data_port, producer))?;
                    Some(display_err!(AudioPlayer::start_playback(
                        Some(mic.server_device_index),
                        consumer,
                    ))?)
                }
                Switch::Disabled => None,
            };

            let (present_producer, present_consumer) = queue_channel_split();
            let sync_handle_mutex = Arc::new(Mutex::new(()));

            display_err!(compositor.lock().initialize_for_client(
                target_eye_width,
                target_eye_height,
                settings.video.foveated_rendering.clone().into_option(),
                present_consumer,
                sync_handle_mutex.clone(),
                slice_producers,
            ))?;

            openvr_backend
                .lock()
                .initialize_for_client_or_request_restart(
                    &settings,
                    session_desc_loader.lock().get_mut(),
                    present_producer,
                    sync_handle_mutex,
                    {
                        let connection_manager = connection_manager.clone();
                        move |haptic_data| {
                            connection_manager
                                .lock()
                                .send_message_udp(&ServerMessage::Haptic(haptic_data));
                        }
                    },
                );

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
                    .send_message_tcp(&ServerMessage::Shutdown);
            }

            // Dropping an object that contains a thread loop requires waiting for some actions to
            // timeout. The drops happen sequentially so the time required to execute them is at
            // worst the sum of all timeouts. By calling request_stop() on all objects involved I
            // can buffer all the shutdown requests at once, so if we drop the objects immediately
            // after, the time needed for all drops is at worst the maximum of all the timeouts.

            connection_manager.lock().request_stop();

            if let Some(recorder) = &mut maybe_game_audio_recorder {
                recorder.request_stop();
            }

            if let Some(player) = &mut maybe_microphone_player {
                player.request_stop();
            }

            compositor.lock().request_deinitialize_for_client();

            res
        }
    };

    thread::spawn(move || {
        while Instant::now() < deadline {
            match try_connect(&shutdown_signal_receiver) {
                Ok(ShutdownSignal::ClientDisconnected) => deadline = Instant::now() + timeout,
                Ok(ShutdownSignal::BackendShutdown) => break,
                Err(()) => {
                    if let Ok(ShutdownSignal::BackendShutdown) | Err(TryRecvError::Disconnected) =
                        shutdown_signal_receiver.try_recv()
                    {
                        break;
                    }
                }
            }
            openvr_backend.lock().deinitialize_for_client();
            compositor.lock().request_deinitialize_for_client();
        }
    });

    Ok(())
}

// To make a minimum system, BridgeVR needs to instantiate Compositor and OpenvrServer.
// This means that most OpenVR related settings cannot be changed while the driver is running.
// OpenvrServer needs to be instantiated statically because if it get destroyed SteamVR will find
// invalid pointers.
// Avoid crashing or returning errors, otherwise SteamVR would complain that there is no HMD.
// If get_settings() returns an error, create the OpenVR server anyway, even if it remains in an
// unusable state. If the compositor can't be created, there is nothing to do and HmdFactory
// will return a null pointer.

type Temp<T> = Arc<Mutex<Option<T>>>;

struct EmptySystem {
    compositor: Arc<Mutex<Compositor>>,
    openvr_backend: Arc<Mutex<OpenvrBackend>>,
    shutdown_signal_channel_tmp: Temp<ShutdownSignalChannel>,
    session_desc_loader: Arc<Mutex<SessionDescLoader>>,
}

fn create_empty_system() -> StrResult<EmptySystem> {
    let maybe_settings = get_settings()
        .map_err(|_| warn!("Cannot read settings. BridgeVR server will be in an invalid state."))
        .ok();

    let session_desc_loader = Arc::new(Mutex::new(SessionDescLoader::load(env!("SESSION_PATH"))));

    let compositor = Arc::new(Mutex::new(Compositor::new()?));

    let (shutdown_signal_sender, shutdown_signal_receiver) = mpsc::channel();

    let openvr_backend = Arc::new(Mutex::new(OpenvrBackend::new(
        maybe_settings.as_ref(),
        &session_desc_loader.lock().get_mut(),
        compositor.clone(),
        shutdown_signal_sender.clone(),
    )));

    Ok(EmptySystem {
        compositor,
        openvr_backend,
        shutdown_signal_channel_tmp: Arc::new(Mutex::new(Some((
            shutdown_signal_sender,
            shutdown_signal_receiver,
        )))),
        session_desc_loader,
    })
}

openvr_server_entry_point!({
    logging_backend::init_logging();

    lazy_static! {
        static ref EMPTY_SYSTEM: StrResult<EmptySystem> = create_empty_system();
    }

    display_err!(EMPTY_SYSTEM.as_ref()).map(|sys| {
        let shutdown_signal_channel = sys.shutdown_signal_channel_tmp.lock().take().unwrap();
        display_err!(begin_server_loop(
            sys.compositor.clone(),
            sys.openvr_backend.clone(),
            shutdown_signal_channel,
            sys.session_desc_loader.clone()
        ))
        .ok();

        sys.openvr_backend.lock().server_native()
    })
});
