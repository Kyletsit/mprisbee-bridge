use async_channel::{Receiver, Sender};
use async_std::{fs::read, os::unix::net::UnixListener};
use async_std::{path, prelude::*};
use async_std::sync::RwLock;
use async_std::task;
use futures::{select, stream::All, FutureExt, StreamExt};
use mpris_server::zbus::zvariant;
use mpris_server::{
    builder::MetadataBuilder, zbus::{fdo, zvariant::Type, Message, Result}, LoopStatus, Metadata, PlaybackRate, PlaybackStatus, PlayerInterface, Playlist, PlaylistId, PlaylistOrdering, PlaylistsInterface, Property, RootInterface, Server, Signal, Time, TrackId, TrackListInterface, Uri, Volume
};
use serde_json::Value;
use std::fs::{Permissions, create_dir, set_permissions};
use std::io::Result as IoResult;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::Arc;
use sysinfo::System;
use users::get_current_uid;

#[derive(Debug)]
pub enum SocketCommand {
    MetadataChange(Metadata),
    ArtUpdate(TrackId, Option<String>),
    Seek(Time),
    Pause,
    Play,
    Stop,
    Shuffle(bool),
    LoopStatus(LoopStatus),
    Exit,
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
    SetPosition(TrackId, Time),
    SetLoopStatus(LoopStatus),
    SetShuffle(bool),
    SetVolume(Volume),
}

pub struct SocketHandler {
    command_sender: Sender<SocketCommand>,
    message_receiver: Receiver<MprisMessage>,
    wineprefix: Option<std::string::String>,
    uid: users::uid_t,
}

impl SocketHandler {
    pub fn new(
        command_sender: Sender<SocketCommand>,
        message_receiver: Receiver<MprisMessage>,
    ) -> Self {
        Self {
            command_sender,
            message_receiver,
            wineprefix: None,
            uid: get_current_uid(),
        }
    }

    pub async fn run(&mut self) -> IoResult<()> {
        let socket_dir = format!("/tmp/mprisbee{}", self.uid);
        create_dir(&socket_dir).unwrap_or_else(|e| {
            if e.kind() != std::io::ErrorKind::AlreadyExists {
                panic!("{}", e);
            }
        });
        set_permissions(&socket_dir, Permissions::from_mode(0o700))?;

        let socket_path = format!("{}/wine.sock", socket_dir);
        if Path::new(&socket_path).exists() {
            std::fs::remove_file(&socket_path)?;
        }

        let listener = UnixListener::bind(&socket_path).await.unwrap();
        set_permissions(&socket_path, Permissions::from_mode(0o700))?;

        eprintln!("Socket listening at: {}", socket_path);

        let (stream, _) = listener.accept().await?;

        let mut sys = System::new();
        sys.refresh_processes_specifics(
            sysinfo::ProcessesToUpdate::All,
            true,
            sysinfo::ProcessRefreshKind::nothing().with_environ(sysinfo::UpdateKind::Always)
        );

        if let Some(process) = sys.processes_by_exact_name("MusicBee.exe".as_ref()).next() {
            if let Some(wineprefix) = process.environ().iter()
                .find_map(|var| {
                    let var_str = var.to_string_lossy();
                    if var_str.starts_with("WINEPREFIX=") {
                        Some(var_str["WINEPREFIX=".len()..].to_string())
                    } else {
                        None
                    }
                })
            {
                println!("WINEPREFIX: {}", wineprefix);
                self.wineprefix = wineprefix.into();
            } else {
                println!("WINEPREFIX not found in environment");
            }
        } else {
            println!("No MusicBee.exe process found");
        }

        let reader = async_std::io::BufReader::new(&stream);
        let mut lines = reader.lines();
        let mut writer = &stream;

        loop {
            select! {
                line = lines.next().fuse() => {
                    if let Some(Ok(line)) = line {
                        if let Ok(json) = serde_json::from_str::<Value>(&line) {
                            println!("IN: Recieved {:?}", json.get("event"));
                            match json.get("event").and_then(|e| e.as_str()) {
                                Some("metachange") => {
                                    println!("IN: Metadata {:?}", json);
                                    if let Some(metadata_json) = json.get("metadata") {
                                        let trackid_str = metadata_json.get("trackid").and_then(|a| a.as_str()).unwrap_or("/org/musicbee/track/unknown");
                                        let title = metadata_json.get("title").and_then(|a| a.as_str()).unwrap_or("Unknown Title");
                                        let length = Time::from_millis(metadata_json.get("length").and_then(|a| a.as_i64()).unwrap_or(0));
                                        let artist: Vec<String> = metadata_json
                                            .get("artist")
                                            .and_then(|a| a.as_array())
                                            .map(|arr| {
                                                arr.iter()
                                                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                                                .collect()
                                            })
                                            .filter(|v: &Vec<String>| !v.is_empty())
                                            .unwrap_or_else(|| vec!["Unknown Artist".to_string()]);
                                        let album = metadata_json.get("album").and_then(|a| a.as_str()).unwrap_or("Unknown Album");
                                        let disc_number =  metadata_json.get("disc_number").and_then(|a| a.as_i64());
                                        let track_number = metadata_json.get("track_number").and_then(|a| a.as_i64());
                                        let album_artist: Option<Vec<String>> = metadata_json
                                            .get("album_artist")
                                            .and_then(|a| a.as_array())
                                            .map(|arr| {
                                                arr.iter()
                                                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                                                .collect()
                                            });
                                        let composer: Option<Vec<String>> = metadata_json
                                            .get("composer")
                                            .and_then(|a| a.as_array())
                                            .map(|arr| {
                                                arr.iter()
                                                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                                                .collect()
                                            });
                                        let lyricist: Option<Vec<String>> = metadata_json
                                            .get("lyricist")
                                            .and_then(|a| a.as_array())
                                            .map(|arr| {
                                                arr.iter()
                                                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                                                .collect()
                                            });
                                        let genre: Option<Vec<String>> = metadata_json
                                            .get("genre")
                                            .and_then(|a| a.as_array())
                                            .map(|arr| {
                                                arr.iter()
                                                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                                                .collect()
                                            });
                                        let bpm = metadata_json.get("bpm").and_then(|a| a.as_i64());
                                        let content_created = metadata_json.get("content_created").and_then(|a| a.as_str());
                                        let rating = metadata_json.get("rating").and_then(|a| a.as_f64());
                                        let comment: Option<Vec<String>> = metadata_json
                                            .get("comment")
                                            .and_then(|a| a.as_array())
                                            .map(|arr| {
                                                arr.iter()
                                                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                                                .collect()
                                            });
                                        let file_url = metadata_json
                                            .get("file_url")
                                            .and_then(|a| a.as_str())
                                            .map(|path| {
                                                "file://".to_owned()
                                                    + &path
                                                        .replace('\\', "/")
                                                        .replace("C:/", &format!("{}drive_c/", self.wineprefix.clone().unwrap()) )
                                                        .replace("Z:/", "/")
                                            })
                                            .unwrap_or_else(|| "Unknown File URL".to_string());

                                         let mut builder = Metadata::builder()
                                            .trackid(TrackId::try_from(trackid_str).unwrap_or_else(|_| TrackId::default()))
                                            .title(title)
                                            .length(length)
                                            .artist(artist)
                                            .album(album)
                                            .url(file_url);

                                        macro_rules! set_if_some {
                                            ($builder:expr, $method:ident, $opt:expr) => {
                                                if let Some(val) = $opt {
                                                    $builder = $builder.$method(val);
                                                }
                                            };
                                        }

                                        set_if_some!(builder, disc_number, disc_number.and_then(|x| x.try_into().ok()));
                                        set_if_some!(builder, track_number, track_number.and_then(|x| x.try_into().ok()));
                                        set_if_some!(builder, album_artist, album_artist);
                                        set_if_some!(builder, composer, composer);
                                        set_if_some!(builder, lyricist, lyricist);
                                        set_if_some!(builder, genre, genre);
                                        set_if_some!(builder, audio_bpm, bpm.and_then(|x| x.try_into().ok()));
                                        set_if_some!(builder, content_created, content_created);
                                        set_if_some!(builder, user_rating, rating);
                                        set_if_some!(builder, comment, comment);

                                        let metadata = builder.build();

                                        let _ = self.command_sender.send(SocketCommand::MetadataChange(metadata)).await;
                                    }
                                }
                                Some("artupdate") => {
                                    let trackid_str = json.get("trackid").and_then(|a| a.as_str()).unwrap_or("/org/musicbee/track/unknown");
                                    let track_id = TrackId::try_from(trackid_str).unwrap_or_else(|_| TrackId::default());
                                    let albumart_url = json
                                        .get("albumartpath")
                                        .and_then(|a| a.as_str())
                                        .map(|path| {
                                            "file://".to_owned()
                                                + &path
                                                    .replace('\\', "/")
                                                    .replace("C:/", &format!("{}drive_c/", self.wineprefix.clone().unwrap()) )
                                                    .replace("Z:/", "/")
                                        });

                                    let _ = self.command_sender.send(SocketCommand::ArtUpdate(track_id, albumart_url)).await;

                                }
                                Some("seek") => {
                                    if let Some(offset) = json.get("offset").and_then(|d| d.as_i64()) {
                                        let _ = self.command_sender.send(SocketCommand::Seek(Time::from_millis(offset))).await;
                                    }
                                }
                                Some("pause") => {
                                    let _ = self.command_sender.send(SocketCommand::Pause).await;
                                }
                                Some("play") => {
                                    let _ = self.command_sender.send(SocketCommand::Play).await;
                                }
                                Some("stop") => {
                                    let _ = self.command_sender.send(SocketCommand::Stop).await;
                                }
                                Some("shuffle") => {
                                    if let Some(shuffle) = json.get("status").and_then(|b| b.as_bool()) {
                                        let _ = self.command_sender.send(SocketCommand::Shuffle(shuffle)).await;
                                    }
                                }
                                Some("loopstatus") => {
                                    if let Some(status_str) = json.get("status").and_then(|s| s.as_str()) {
                                        let val = zvariant::Value::from(status_str);
                                        let lst = LoopStatus::try_from(val).unwrap_or(LoopStatus::None);
                                        let _ = self.command_sender.send(SocketCommand::LoopStatus(lst)).await;
                                    }
                                }
                                Some("exit") => {
                                    let _ = self.command_sender.send(SocketCommand::Exit).await;
                                    break;
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
                            MprisMessage::Seek(offset) => serde_json::json!({"event":"seek","offset":offset.as_micros()}),
                            MprisMessage::SetPosition(trackid, position) => serde_json::json!({"event":"position","trackid":trackid,"position":position.as_micros()}),
                            MprisMessage::SetLoopStatus(loop_status) => serde_json::json!({"event":"loop_status","status":loop_status.as_str()}),
                            MprisMessage::SetShuffle(shuffle) => serde_json::json!({"event":"shuffle","shuffle":shuffle}),
                            MprisMessage::SetVolume(volume) => serde_json::json!({"event":"volume","volume":volume}),
                                                    };
                        println!("OUT: {}", json);
                        if let Err(e) = writer.write_all(format!("{}\n", json).as_bytes()).await {
                            eprintln!("Failed to write to socket: {}", e);
                        }
                        if let Err(e) = writer.flush().await {
                            eprintln!("Failed to flush socket: {}", e);
                        }
                    }
                }
            }
        }

        println!("Broke out of the SocketHandler loop");
        Ok(())
    }
}

impl Drop for SocketHandler {
    fn drop(&mut self) {
            let socket_dir = format!("/tmp/mprisbee{}", self.uid);
            let socket_path = format!("{}/wine-out", socket_dir);

            println!("Removing {}", socket_path);
            std::fs::remove_file(socket_path).unwrap_or_else(|e| {
                if e.kind() != std::io::ErrorKind::NotFound {
                    panic!("{}", e);
                }
            });

            println!("Removing {}", socket_dir);
            std::fs::remove_dir(socket_dir).unwrap_or_else(|e| {
                if e.kind() != std::io::ErrorKind::NotFound {
                    panic!("{}", e);
                }
            });
        }
}

struct PlayerState {
    playback_status: PlaybackStatus,
    loop_status: LoopStatus,
    shuffle: bool,
    metadata: Metadata,
    volume: Volume,
    position: Time,
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
            shuffle: false,
            metadata: Metadata::default(),
            volume: Volume::default(),
            position: Time::default(),
            can_go_next: true,
            can_go_previous: true,
            can_play: true,
            can_pause: true,
            can_seek: true,
        }
    }
}

impl PlayerState {
    fn switch_playback_status(&mut self) {
        match self.playback_status {
            PlaybackStatus::Paused | PlaybackStatus::Stopped => {
                self.playback_status = PlaybackStatus::Playing;
            }
            PlaybackStatus::Playing => {
                self.playback_status = PlaybackStatus::Paused;
            }
        };
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
        println!("CanQuit: false");
        Ok(false)
    }

    async fn fullscreen(&self) -> fdo::Result<bool> {
        println!("Fullscreen: false");
        Ok(false)
    }

    async fn set_fullscreen(&self, fullscreen: bool) -> Result<()> {
        println!("SetFullscreen({})", fullscreen);
        Ok(())
    }

    async fn can_set_fullscreen(&self) -> fdo::Result<bool> {
        println!("CanSetFullscreen: false");
        Ok(false)
    }

    async fn can_raise(&self) -> fdo::Result<bool> {
        println!("CanRaise: true");
        Ok(true)
    }

    async fn has_track_list(&self) -> fdo::Result<bool> {
        println!("HasTrackList: false");
        Ok(false)
    }

    async fn identity(&self) -> fdo::Result<String> {
        println!("Identity: MusicBee");
        Ok("MusicBee".into())
    }

    async fn desktop_entry(&self) -> fdo::Result<String> {
        println!("DesktopEntry: MusicBee");
        Ok("MusicBee".into())
    }

    async fn supported_uri_schemes(&self) -> fdo::Result<Vec<String>> {
        println!("SupportedUriSchemes: [file]");
        Ok(vec!["file".into()])
    }

    async fn supported_mime_types(&self) -> fdo::Result<Vec<String>> {
        println!("SupportedMimeTypes: []");
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
        self.state.read().await.playback_status;

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
        Ok(1.0)
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
        Ok(1.0)
    }

    async fn maximum_rate(&self) -> fdo::Result<PlaybackRate> {
        println!("MaximumRate");
        Ok(1.0)
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
        println!("CanControl: true");
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

    let mut socket_handler = SocketHandler::new(socket_to_mpris_tx.clone(), mpris_to_socket_rx);
    let socket_handler_handle = task::spawn(async move {
        socket_handler.run().await.unwrap();
        println!("SocketHandler finished, deleting socket");
    });

    let command_handler_handle = task::spawn(async move {
        while let Ok(cmd) = socket_to_mpris_rx.recv().await {
            match cmd {
                SocketCommand::MetadataChange(metadata) => {
                    let mut state_w = state_clone.write().await;
                    state_w.metadata = metadata.clone();
                    server.properties_changed([
                        Property::Metadata(metadata)
                    ]).await.unwrap();
                },
                SocketCommand::ArtUpdate(trackid, art_url) => {
                    let mut state_w = state_clone.write().await;
                    if let Some(player_trackid) = state_w.metadata.trackid() {
                        if player_trackid == trackid {
                            state_w.metadata.set_art_url(art_url);
                            server.properties_changed([
                                Property::Metadata(state_w.metadata.clone())
                            ]).await.unwrap();
                        } else {
                            println!("Recieved album art is not for the currently playing song.");
                        }
                    }
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
                SocketCommand::Pause => {
                    let mut state_w = state_clone.write().await;
                    state_w.playback_status = PlaybackStatus::Paused;
                    server
                        .properties_changed([
                            Property::PlaybackStatus(PlaybackStatus::Paused)
                        ]).await.unwrap();
                },
                SocketCommand::Play => {
                    let mut state_w = state_clone.write().await;
                    state_w.playback_status = PlaybackStatus::Playing;
                    server
                        .properties_changed([
                            Property::PlaybackStatus(PlaybackStatus::Playing)
                        ]).await.unwrap();
                },
                SocketCommand::Stop => {
                    let mut state_w = state_clone.write().await;
                    state_w.playback_status = PlaybackStatus::Stopped;
                    server
                        .properties_changed([
                            Property::PlaybackStatus(PlaybackStatus::Stopped)
                        ]).await.unwrap();
                },
                SocketCommand::Shuffle(shuffle) => {
                    let mut state_w = state_clone.write().await;
                    state_w.shuffle = shuffle;
                    server
                        .properties_changed([
                            Property::Shuffle(shuffle)
                        ]).await.unwrap();
                },
                SocketCommand::LoopStatus(status) => {
                    let mut state_w = state_clone.write().await;
                    state_w.loop_status = status;
                    server
                        .properties_changed([
                            Property::LoopStatus(status)
                        ]).await.unwrap();
                },
                SocketCommand::Exit => {
                    break;
                },
            }
        }

        println!("Broke out of the SocketCommand handler loop");
    });

    println!("Main waiting for SocketCommand handler to finish...");
    task::block_on(command_handler_handle);
    println!("SocketCommand handler finished");
    println!("Main waiting for SocketHandler to finish...");
    task::block_on(socket_handler_handle);
    println!("SocketHandler finished, Main concluding");
    Ok(())
}
