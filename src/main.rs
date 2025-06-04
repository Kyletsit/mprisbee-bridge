use async_channel::{Receiver, Sender};
use async_std::os::unix::net::UnixListener;
use async_std::prelude::*;
use async_std::sync::RwLock;
use async_std::task;
use futures::{FutureExt, StreamExt, select};
use mpris_server::{
    builder::MetadataBuilder, zbus::{fdo, Result}, LoopStatus, Metadata, PlaybackRate, PlaybackStatus, PlayerInterface, Playlist, PlaylistId, PlaylistOrdering, PlaylistsInterface, Property, RootInterface, Server, Signal, Time, TrackId, TrackListInterface, Uri, Volume
};
use serde_json::Value;
use std::fs::{Permissions, create_dir, set_permissions};
use std::io::Result as IoResult;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::Arc;
use users::get_current_uid;

#[derive(Debug)]
pub enum SocketCommand {
    TrackChange(Metadata),
    Seek(Time),
    PlayPause,
}

#[derive(Debug)]
pub enum MprisMessage {
    Next,
    Previous,
    Pause,
    PlayPause,
    Stop,
    Play,
    Seek(Time),
}

pub struct SocketHandler {
    command_sender: Sender<SocketCommand>,
    message_receiver: Receiver<MprisMessage>,
}

impl SocketHandler {
    pub fn new(
        command_sender: Sender<SocketCommand>,
        message_receiver: Receiver<MprisMessage>,
    ) -> Self {
        Self {
            command_sender,
            message_receiver,
        }
    }

    pub async fn run(&self) -> IoResult<()> {
        let socket_dir = format!("/tmp/mprisbee{}", get_current_uid());
        create_dir(&socket_dir).unwrap_or_else(|e| {
            if e.kind() != std::io::ErrorKind::AlreadyExists {
                panic!("{}", e);
            }
        });
        set_permissions(&socket_dir, Permissions::from_mode(0o700))?;

        let socket_path = format!("{}/wine-out", socket_dir);
        if Path::new(&socket_path).exists() {
            std::fs::remove_file(&socket_path)?;
        }

        let listener = UnixListener::bind(&socket_path).await.unwrap();
        set_permissions(&socket_path, Permissions::from_mode(0o700))?;

        eprintln!("Socket listening at: {}", socket_path);

        let (stream, _) = listener.accept().await?;
        let reader = async_std::io::BufReader::new(&stream);
        let mut lines = reader.lines();
        let mut writer = &stream;

        loop {
            select! {
                line = lines.next().fuse() => {
                    if let Some(Ok(line)) = line {
                        if let Ok(json) = serde_json::from_str::<Value>(&line) {
                            match json.get("event").and_then(|e| e.as_str()) {
                                Some("trackchange") => {
                                    if let Some(metadata_json) = json.get("metadata") {
                                        let trackid_str = metadata_json.get("trackid").and_then(|t| t.as_str()).unwrap_or("/org/musicbee/track/unknown");
                                        let title = metadata_json.get("title").and_then(|t| t.as_str()).unwrap_or("Unknown Title");
                                        let artist = metadata_json.get("artist").and_then(|a| a.as_str()).unwrap_or("Unknown Artist");

                                        let metadata = Metadata::builder()
                                            .trackid(TrackId::try_from(trackid_str).unwrap_or_else(|_| TrackId::default()))
                                            .title(title)
                                            .artist(vec![artist.to_string()])
                                            .build();

                                        let _ = self.command_sender.send(SocketCommand::TrackChange(metadata)).await;
                                    }
                                }
                                Some("seek") => {
                                    if let Some(duration) = json.get("duration").and_then(|d| d.as_i64()) {
                                        let _ = self.command_sender.send(SocketCommand::Seek(Time::from_millis(duration))).await;
                                    }
                                }
                                Some("playpause") => {
                                    let _ = self.command_sender.send(SocketCommand::PlayPause).await;
                                }
                                Some(other) => {
                                    eprintln!("Unknown event: {}", other);
                                }
                                None => {
                                    eprintln!("Missing event field in JSON");
                                }
                            }
                        }
                    } else {
                        break;
                    }
                },
                msg = self.message_receiver.recv().fuse() => {
                    if let Ok(message) = msg {
                        let json = match message {
                            MprisMessage::Next => serde_json::json!({"event":"next"}),
                            MprisMessage::Previous => serde_json::json!({"event":"previous"}),
                            MprisMessage::Pause => serde_json::json!({"event":"pause"}),
                            MprisMessage::PlayPause => serde_json::json!({"event":"playpause"}),
                            MprisMessage::Stop => serde_json::json!({"event":"stop"}),
                            MprisMessage::Play => serde_json::json!({"event":"play"}),
                            MprisMessage::Seek(duration) => serde_json::json!({"event":"seek", "duration":duration}),
                        };
                        if let Err(e) = writer.write_all(json.to_string().as_bytes()).await {
                            eprintln!("Failed to write to socket: {}", e);
                        }
                        if let Err(e) = writer.write_all(b"\n").await {
                            eprintln!("Failed to write \\n to socket: {}", e);
                        }
                        if let Err(e) = writer.flush().await {
                            eprintln!("Failed to flush socket: {}", e);
                        }
                    }
                }
            }
        }

        Ok(())
    }
}

struct PlayerState {
    playback_status: PlaybackStatus,
    loop_status: LoopStatus,
    playback_rate: PlaybackRate,
    shuffle: bool,
    metadata: Metadata,
    volume: Volume,
    position: Time,
    minimum_rate: PlaybackRate,
    maximum_rate: PlaybackRate,
    can_go_next: bool,
    can_go_previous: bool,
    can_play: bool,
    can_pause: bool,
    can_seek: bool,
}

impl Default for PlayerState {
    fn default() -> Self {
        Self {
            playback_status: PlaybackStatus::Stopped,
            loop_status: LoopStatus::None,
            playback_rate: 1.0,
            shuffle: false,
            metadata: Metadata::default(),
            volume: Volume::default(),
            position: Time::default(),
            minimum_rate: 0.1,
            maximum_rate: 10.0,
            can_go_next: true,
            can_go_previous: false,
            can_play: true,
            can_pause: false,
            can_seek: true,
        }
    }
}

struct Player {
    state: Arc<RwLock<PlayerState>>,
    message_sender: Sender<MprisMessage>,
}

impl RootInterface for Player {
    async fn raise(&self) -> fdo::Result<()> {
        println!("Raise");
        Ok(())
    }

    async fn quit(&self) -> fdo::Result<()> {
        println!("Quit");
        Ok(())
    }

    async fn can_quit(&self) -> fdo::Result<bool> {
        println!("CanQuit");
        Ok(false)
    }

    async fn fullscreen(&self) -> fdo::Result<bool> {
        println!("Fullscreen");
        Ok(false)
    }

    async fn set_fullscreen(&self, fullscreen: bool) -> Result<()> {
        println!("SetFullscreen({})", fullscreen);
        Ok(())
    }

    async fn can_set_fullscreen(&self) -> fdo::Result<bool> {
        println!("CanSetFullscreen");
        Ok(false)
    }

    async fn can_raise(&self) -> fdo::Result<bool> {
        println!("CanRaise");
        Ok(true)
    }

    async fn has_track_list(&self) -> fdo::Result<bool> {
        println!("HasTrackList");
        Ok(false)
    }

    async fn identity(&self) -> fdo::Result<String> {
        println!("Identity");
        Ok("MusicBee".into())
    }

    async fn desktop_entry(&self) -> fdo::Result<String> {
        println!("DesktopEntry");
        Ok("MusicBee".into())
    }

    async fn supported_uri_schemes(&self) -> fdo::Result<Vec<String>> {
        println!("SupportedUriSchemes");
        Ok(vec!["file".into()])
    }

    async fn supported_mime_types(&self) -> fdo::Result<Vec<String>> {
        println!("SupportedMimeTypes");
        Ok(vec![])
    }
}

impl PlayerInterface for Player {
    async fn next(&self) -> fdo::Result<()> {
        if let Err(e) = self.message_sender.send(MprisMessage::Next).await {
            eprintln!("Failed to send messge to SocketHandler: {}", e);
        }

        println!("Next message sent to SocketHandler");
        Ok(())
    }

    async fn previous(&self) -> fdo::Result<()> {
        if let Err(e) = self.message_sender.send(MprisMessage::Previous).await {
            eprintln!("Failed to send messge to SocketHandler: {}", e);
        }

        println!("Previous message sent to SocketHandler");
        Ok(())
    }

    async fn pause(&self) -> fdo::Result<()> {
        if let Err(e) = self.message_sender.send(MprisMessage::Pause).await {
            eprintln!("Failed to send messge to SocketHandler: {}", e);
        }

        println!("Pause message sent to SocketHandler");
        Ok(())
    }

    async fn play_pause(&self) -> fdo::Result<()> {
        if let Err(e) = self.message_sender.send(MprisMessage::PlayPause).await {
            eprintln!("Failed to send messge to SocketHandler: {}", e);
        }

        println!("PlayPause message sent to SocketHandler");
        Ok(())
    }

    async fn stop(&self) -> fdo::Result<()> {
        if let Err(e) = self.message_sender.send(MprisMessage::Stop).await {
            eprintln!("Failed to send messge to SocketHandler: {}", e);
        }

        println!("Stop message sent to SocketHandler");
        Ok(())
    }

    async fn play(&self) -> fdo::Result<()> {
        if let Err(e) = self.message_sender.send(MprisMessage::Play).await {
            eprintln!("Failed to send messge to SocketHandler: {}", e);
        }

        println!("Play message sent to SocketHandler");
        Ok(())
    }

    async fn seek(&self, offset: Time) -> fdo::Result<()> {
        if let Err(e) = self.message_sender.send(MprisMessage::Seek(offset)).await {
            eprintln!("Failed to send messge to SocketHandler: {}", e);
        }

        println!("Seek message sent to SocketHandler");
        Ok(())
    }

    async fn set_position(&self, track_id: TrackId, position: Time) -> fdo::Result<()> {
        println!("SetPosition({}, {:?})", track_id, position);
        Ok(())
    }

    async fn open_uri(&self, uri: String) -> fdo::Result<()> {
        println!("OpenUri({})", uri);
        Ok(())
    }

    async fn playback_status(&self) -> fdo::Result<PlaybackStatus> {
        println!("PlaybackStatus");
        Ok(self.state.read().await.playback_status)
    }

    async fn loop_status(&self) -> fdo::Result<LoopStatus> {
        println!("LoopStatus");
        Ok(self.state.read().await.loop_status)
    }

    async fn set_loop_status(&self, loop_status: LoopStatus) -> Result<()> {
        println!("SetLoopStatus({})", loop_status);
        Ok(())
    }

    async fn rate(&self) -> fdo::Result<PlaybackRate> {
        println!("Rate");
        Ok(self.state.read().await.playback_rate)
    }

    async fn set_rate(&self, rate: PlaybackRate) -> Result<()> {
        println!("SetRate({})", rate);
        Ok(())
    }

    async fn shuffle(&self) -> fdo::Result<bool> {
        println!("Shuffle");
        Ok(self.state.read().await.shuffle)
    }

    async fn set_shuffle(&self, shuffle: bool) -> Result<()> {
        println!("SetShuffle({})", shuffle);
        Ok(())
    }

    async fn metadata(&self) -> fdo::Result<Metadata> {
        println!("Metadata");
        Ok(self.state.read().await.metadata.clone())
    }

    async fn volume(&self) -> fdo::Result<Volume> {
        println!("Volume");
        Ok(self.state.read().await.volume)
    }

    async fn set_volume(&self, volume: Volume) -> Result<()> {
        println!("SetVolume({})", volume);
        Ok(())
    }

    async fn position(&self) -> fdo::Result<Time> {
        println!("Position");
        Ok(self.state.read().await.position)
    }

    async fn minimum_rate(&self) -> fdo::Result<PlaybackRate> {
        println!("MinimumRate");
        Ok(self.state.read().await.minimum_rate)
    }

    async fn maximum_rate(&self) -> fdo::Result<PlaybackRate> {
        println!("MaximumRate");
        Ok(self.state.read().await.maximum_rate)
    }

    async fn can_go_next(&self) -> fdo::Result<bool> {
        println!("CanGoNext");
        Ok(self.state.read().await.can_go_next)
    }

    async fn can_go_previous(&self) -> fdo::Result<bool> {
        println!("CanGoPrevious");
        Ok(self.state.read().await.can_go_previous)
    }

    async fn can_play(&self) -> fdo::Result<bool> {
        println!("CanPlay");
        Ok(self.state.read().await.can_play)
    }

    async fn can_pause(&self) -> fdo::Result<bool> {
        println!("CanPause");
        Ok(self.state.read().await.can_pause)
    }

    async fn can_seek(&self) -> fdo::Result<bool> {
        println!("CanSeek");
        Ok(self.state.read().await.can_seek)
    }

    async fn can_control(&self) -> fdo::Result<bool> {
        println!("CanControl");
        Ok(true)
    }
}

#[async_std::main]
async fn main() -> IoResult<()> {
    let (socket_to_mpris_tx, socket_to_mpris_rx) = async_channel::unbounded::<SocketCommand>();
    let (mpris_to_socket_tx, mpris_to_socket_rx) = async_channel::unbounded::<MprisMessage>();

    let player = Player {
        state: Arc::new(RwLock::new(PlayerState::default())),
        message_sender: mpris_to_socket_tx.clone(),
    };
    let state_clone = Arc::clone(&player.state);

    let server = Server::new("MusicBee", player).await.unwrap();

    let handler = SocketHandler::new(socket_to_mpris_tx.clone(), mpris_to_socket_rx);
    task::spawn(async move {
        handler.run().await.unwrap();
    });

    task::spawn(async move {
        while let Ok(cmd) = socket_to_mpris_rx.recv().await {
            match cmd {
                SocketCommand::TrackChange(metadata) => {
                    let mut state_w = state_clone.write().await;
                    state_w.metadata = metadata.clone();
                    server.properties_changed([
                        Property::Metadata(metadata)
                    ]).await.unwrap();
                },
                SocketCommand::Seek(time) => {
                    let mut state_w = state_clone.write().await;
                    state_w.position = time;
                    server
                        .emit(Signal::Seeked {
                            position: time,
                        })
                        .await.unwrap();
                },
                SocketCommand::PlayPause => {
                    let pl_status = {
                        let state_r = state_clone.read().await;
                        state_r.playback_status.clone()
                    };

                    let mut state_w = state_clone.write().await;
                     match pl_status {
                        PlaybackStatus::Paused | PlaybackStatus::Stopped => {
                            state_w.playback_status = PlaybackStatus::Playing;
                            server
                                .properties_changed([
                                    Property::PlaybackStatus(PlaybackStatus::Playing)
                                ]).await.unwrap();
                        }
                        PlaybackStatus::Playing => {
                            state_w.playback_status = PlaybackStatus::Paused;
                            server
                                .properties_changed([
                                    Property::PlaybackStatus(PlaybackStatus::Paused)
                                ]).await.unwrap();
                        }
                    };
                },
            }
        }
    });

    async_std::future::pending::<()>().await;
    Ok(())
}
