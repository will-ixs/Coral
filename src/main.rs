#![windows_subsystem = "windows"]
use rodio::{Decoder, OutputStream, Sink, Source};
use egui::{ahash::HashMap, text::{LayoutJob, TextFormat}, Align, Color32, IconData, TextEdit, ViewportBuilder};
use egui_dnd::{self};
use eframe::{egui, Storage, NativeOptions};
use core::{f32};
use lofty::{tag, probe::Probe, file::TaggedFileExt};
use image::GenericImageView;
use std::{fs::File, io::BufReader, path::PathBuf, sync::Arc, time::Duration};
use rand::{rng, seq::SliceRandom};
use discord_rich_presence::{activity::{self, Assets}, DiscordIpc, DiscordIpcClient};


fn main() -> eframe::Result {

    let img = image::open("assets/grad2.png").expect("Failed to open icon");
    let (width, height) = img.dimensions();
    let rgba = img.to_rgba8().into_raw();

    let icon = IconData {
        rgba,
        width,
        height,
    };
    
    let options = NativeOptions {
        viewport: ViewportBuilder::default().with_icon(icon),
        ..Default::default()
    };
    egui::IconData::default();

    return eframe::run_native("Coral", options, Box::new(|cc| Ok(Box::new(PlayerApp::new(cc)))));
}

#[derive(Clone, Default)]
struct SongInfo {
    artist: String,
    track: String,
    album: String,
    path: PathBuf,
    duration: Duration,
    track_number: Option<usize>
}

#[derive(Clone, Default)]
struct AlbumInfo{
    songs: Vec<usize>
}

#[derive(Clone, Default)]
struct LibraryInfo{
    albums: HashMap<(String, String), AlbumInfo>
}

#[derive(Clone, Default, Hash)]
struct QueueEntry{
    song_index: usize,
    uid: usize
}

struct PlayerApp {
    playing: bool,
    volume: f32,
    song_info: Vec<SongInfo>,               //list of all songs recognized by player, paths, names durations, cached
    song_current_position: Option<usize>,   //index of the track currently playing, None when nothing playing
    
    queue_indices: Vec<QueueEntry>,         //list of indices into cached song info, for quicker deletion/addition and no duplicated info
    queue_next_uid: usize,                  //uid for egui_dnd's sorting
    queue_current_position: usize,          //index into the list of indices, stores where we are in that queue of indices// None when queue is empty and nothing playing
    progress: f32, // 0.0–1.0
    
    filter_text: String,
    enabled_album: HashMap<(String, String), bool>,
    show_dirs: bool,
    dirs: Vec<PathBuf>,
    library: LibraryInfo,

    discord_client: DiscordIpcClient,

    _output_stream: OutputStream,
    audio_sink: Sink
}

impl PlayerApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let mut s = Self::default();
        
        if let Some(storage) = cc.storage {
            if let Some(joined) = storage.get_string("dirs") {
                s.dirs = joined.split(";").map(PathBuf::from).collect();
            }
            if let Some(vol) = storage.get_string("vol"){
                s.volume = vol.parse::<f32>().unwrap();
            }
        }

        for dir in s.dirs.clone(){
            s.scan_folder(dir);
        }
        for album in s.library.albums.clone(){
            let mut album_songs = album.1.songs.clone();

            album_songs.sort_by(|a, b| {
                s.song_info[*a].track_number.unwrap_or(usize::MAX).cmp(&s.song_info[*b].track_number.unwrap_or(usize::MAX))
            });
            s.library.albums.entry(album.0).and_modify(|a| a.songs = album_songs);
        }

        let mut fonts = egui::FontDefinitions::default();
        let font_data = std::fs::read("assets/FiraMono-Regular.ttf").expect("Failed to read font file.");

        fonts.font_data.insert("FiraMono".to_owned(), Arc::new(egui::FontData::from_owned(font_data)));
        fonts.families.get_mut(&egui::FontFamily::Proportional).unwrap().insert(0, "FiraMono".to_owned()); 
        fonts.families.get_mut(&egui::FontFamily::Monospace).unwrap().insert(0, "FiraMono".to_owned());
        cc.egui_ctx.set_fonts(fonts);

        s
    }

    fn queue_song_from_file(&mut self, filename: PathBuf){
        let file = File::open(filename);
        if file.is_ok() {
            let dec = Decoder::try_from(file.unwrap()).unwrap();
            self.audio_sink.append(dec);
        }
    }

    fn select_folder_and_scan(&mut self){
        if let Some(folder) = rfd::FileDialog::new().pick_folder() {
            println!("Selected folder: {:?}", folder);
            if let Some(_) = self.dirs.iter().position(|s| *s == folder){
                println!("Directory already exists.");
            }else{
                self.dirs.push(folder.clone());
                self.scan_folder(folder);
            }
        }
    }

    fn scan_folder(&mut self, path: PathBuf){
        if let Ok(entries) = std::fs::read_dir(&path) {
            for entry in entries.flatten() {
                let mut new_song = SongInfo::default();
                
                let path = entry.path();
                let tagged_file = match Probe::open(path.clone()).unwrap().guess_file_type().unwrap().read()
                {
                    Ok(file) => file,
                    Err(_) => {
                        continue;
                    }
                };
                if let Some(prim_tag) = tagged_file.primary_tag() {
                    new_song.artist = prim_tag.get_string(&tag::ItemKey::TrackArtist).unwrap_or("Unknown Artist").to_string();
                    new_song.track = prim_tag.get_string(&tag::ItemKey::TrackTitle).unwrap_or("Unknown Title").to_string();
                    new_song.album = prim_tag.get_string(&tag::ItemKey::AlbumTitle).unwrap_or("Unknown Album").to_string();
                    new_song.track_number = Some(prim_tag.get_string(&tag::ItemKey::TrackNumber).unwrap().parse::<usize>().unwrap_or(usize::MAX));
                } else {
                    let file_name = path.file_stem().and_then(|os| os.to_str()).unwrap_or("Unknown - Unknown").to_string();
                    let artist_title = file_name.split_at_checked(file_name.find("-").expect("rename file to have Artist - Title"));
                    if artist_title.is_some(){
                        new_song.artist = artist_title.unwrap().0.to_string();
                        new_song.track = artist_title.unwrap().1.to_string();
                        new_song.album = "".to_string();
                    }
                }
                new_song.path = path.clone();
                let file = File::open(new_song.path.clone());
                if file.is_ok() {
                    let file_buffer = BufReader::new(file.unwrap());  
                    let source = Decoder::try_from(file_buffer).unwrap();
                    new_song.duration = source.total_duration().unwrap_or(std::time::Duration::from_secs(0));
                }
                let album_artist = new_song.artist.clone().split(|c: char| c == ',' || c == '&' || c == '/').map(|s| s.trim()).find(|s| !s.is_empty()).map(|s| s.to_string());
                let album_key = (new_song.album.clone(), album_artist.unwrap_or(new_song.artist.clone()));
                
                self.enabled_album.insert(album_key.clone(), false);
                self.song_info.push(new_song.clone());
                let song_index = self.song_info.len()-1;
                self.library.albums
                    .entry(album_key.clone())
                    .or_insert(AlbumInfo {
                        songs: Vec::new(),
                    })
                    .songs
                    .push(song_index);
            }
        }
    }
    
    fn play_immediately_with_index(&mut self, index: usize){
        self.audio_sink.clear();
        self.song_current_position = Some(index);
        if let Some(song) = self.song_info.get(index){
            self.queue_song_from_file(song.path.clone());
            self.audio_sink.play();
            self.playing = true;
        }
    }

    fn play_next(&mut self){
        if self.queue_indices.is_empty(){
            self.playing = false;
            return;
        }

        self.progress = 0.0;

        if self.playing{
            self.queue_current_position += 1;
        }

        if self.queue_current_position < self.queue_indices.len(){
            self.playing = true;
            self.song_current_position = Some(self.queue_indices[self.queue_current_position].song_index);
            self.play_immediately_with_index(self.song_current_position.unwrap());
        }else{
            self.playing = false;
            self.song_current_position = None;
        }
    }

    fn add_song_to_queue_with_index(&mut self, index: usize){
        let e = QueueEntry{
            song_index: index,
            uid: self.queue_next_uid,
        };
        self.queue_next_uid += 1;
        self.queue_indices.push(e);
    }

    fn play(&mut self){
        if self.queue_indices.is_empty(){
            self.playing = false;
            return;
        }
        if self.queue_current_position >= self.queue_indices.len() {
            self.queue_current_position = 0;            
            self.song_current_position = Some(self.queue_indices[self.queue_current_position].song_index);
            self.play_immediately_with_index(self.song_current_position.unwrap());
        }
        self.playing = true;
        self.audio_sink.play();
        let song = self.song_info[self.song_current_position.unwrap()].clone();
        self.discord_client.set_activity(activity::Activity::new()
                        .activity_type(activity::ActivityType::Listening)
                        .state(&song.artist.clone())
                        .details(&song.track.clone())
                        .status_display_type(activity::StatusDisplayType::State)
                        .assets(Assets::new()
                            .large_image("grad2")
                            .large_text(&song.album.clone())
                            .small_image("play")
                            .small_text("github.com/will-ixs")
                        )
                    ).expect("Failed to set activity");   
                    println!("Updated playing discord activity");
    }

    fn pause(&mut self){    
        self.playing = false;
        self.audio_sink.pause();
        self.discord_client.set_activity(activity::Activity::new()
                    .activity_type(activity::ActivityType::Listening)
                    .state("Paused...")
                    .details("")
                    .assets(Assets::new()
                        .small_image("pause")
                        .small_text("Paused...")
                    )
                ).expect("Failed to set activity");
                println!("Updated paused discord activity");
    }

    fn back(&mut self){
        //if time close enough to start ,go back in queue, otherwise seek to 0
        if self.progress < 0.1 && self.queue_current_position > 0 {
            //go back in queue
            self.audio_sink.clear();
            self.queue_current_position -= 1;
            let song_index = self.queue_indices[self.queue_current_position].song_index;
            self.play_immediately_with_index(song_index);
        }else{
            self.seek_to(0.0);
        }
    }

    fn ellipsize(text: String, max_chars: usize) -> String{
        if text.chars().count() <= max_chars {
            text
        } else {
            let mut s = text.chars().take(max_chars.saturating_sub(1)).collect::<String>();
            s.push('…');
            s
        }
    }

    fn seek_to(&mut self, seconds: f32){
        let res = self.audio_sink.try_seek(std::time::Duration::from_secs_f32(seconds));
        if res.is_ok(){
            println!("Seeked successfully");
        }else{
            println!("Failed to seek");
            let err = res.unwrap_err();
            println!("{:}", err.to_string());
        }
    }

    fn queue_album(&mut self, song_info: SongInfo) {
        let album_artist = song_info.artist.clone().split(|c: char| c == ',' || c == '&' || c == '/').map(|s| s.trim()).find(|s| !s.is_empty()).map(|s| s.to_string());
        let album_key = (song_info.album.clone(), album_artist.unwrap_or(song_info.artist.clone()));
        let mut album_songs: Option<Vec<usize>> = None;
        if let Some(album) = self.library.albums.get_mut(&album_key) {
            album_songs = Some(album.songs.clone());
        }

        //avoiding double borrow...
        if album_songs.is_some(){
            let mut aos = album_songs.unwrap();
            aos.sort_by(|a, b| {
                self.song_info[*a].track_number.unwrap_or(usize::MAX).cmp(&self.song_info[*b].track_number.unwrap_or(usize::MAX))
            });
    
            for i in 0..aos.len() {
                self.add_song_to_queue_with_index(aos[i]);
            }
        }
    }

    fn shuffle_play(&mut self){
        self.queue_indices = (0..self.song_info.len())
            .map(|i| {
                let e = QueueEntry{
                    song_index: i,
                    uid: self.queue_next_uid
                };
                self.queue_next_uid += 1;
                e
            }).collect();
        self.shuffle_queue();
    }

    fn shuffle_queue(&mut self){
        self.audio_sink.clear();
        let mut rng = rng();
        self.queue_indices.shuffle(&mut rng);
        self.queue_current_position = 0;
        self.play_immediately_with_index(self.queue_indices[0].song_index);
    }

    fn clear_queue(&mut self){
        self.queue_next_uid = 0;
        self.queue_current_position = 0;
        self.queue_indices = Vec::new();
        self.audio_sink.clear();
        self.song_current_position = None;
        self.playing = false;
    }
}

impl Default for PlayerApp {
    fn default() -> Self {
        let ous = rodio::OutputStreamBuilder::open_default_stream().expect("open default audio stream");
        let aus = rodio::Sink::connect_new(&ous.mixer());
        let mut client = DiscordIpcClient::new("1419904366239940719");

        client.connect().expect("Failed to connect");
        client.set_activity(activity::Activity::new()
            .activity_type(activity::ActivityType::Listening)
            .state("")
            .details("")
            .assets(Assets::new()
                .small_image("pause")
                .small_text("Paused")
            )
        ).expect("Failed to set activity");

        Self {
            playing: false,
            volume: 0.5,
            song_info: Vec::new(),
            song_current_position: None,

            queue_indices: Vec::new(),
            queue_next_uid: 0,
            queue_current_position: 0,
            progress: 0.0,

            enabled_album: HashMap::default(),
            show_dirs: false,
            dirs: Vec::new(),
            library: Default::default(),
            filter_text: "".to_string(),

            discord_client: client,
            _output_stream: ous,
            audio_sink: aus
        }
    }
}


impl eframe::App for PlayerApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        //Cache for updates -> discord integration
        let currently_playing: Option<usize> = self.song_current_position;

        if self.audio_sink.empty() {
            self.play_next();
        }

        if self.playing {
            self.progress = self.audio_sink.get_pos().as_secs_f32() / self.song_info[self.song_current_position.unwrap()].duration.as_secs_f32();
        }

        //Top bar, open settings
        egui::TopBottomPanel::top("settings").show(ctx, |ui| {
            ui.horizontal(|ui| {
                if self.song_current_position.is_some(){
                    let song = self.song_info[self.song_current_position.unwrap()].clone();

                    ui.label(song.track + " - " + &song.artist);
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Directories").clicked(){
                        self.show_dirs = true;
                    }
                });
            });
        });

        //Directory management
        if self.show_dirs {
            let mut open = true;
            let mut queue_scan = false;
            let mut remove: Option<usize> = None;
            egui::Window::new("dir_list").open(&mut open)
                .show(ctx, |ui| {
                    if ui.button("Add Directory").clicked() {
                        queue_scan = true;
                    } 
                    
                    ui.label("Directories:");
                    for i in 0..self.dirs.len(){
                        let dir_str = self.dirs[i].to_string_lossy();
                        ui.horizontal(|ui|{
                            if ui.button("X").clicked(){
                                remove = Some(i);
                            }
                            ui.label(dir_str);
                        });
                    }

                });                    
            if remove.is_some(){
                self.dirs.remove(remove.unwrap());
            }
            if queue_scan {
                self.select_folder_and_scan();
            }
            self.show_dirs = open;
        }

        //Player controls
        egui::TopBottomPanel::bottom("Controls").show(ctx, |ui| {
        ui.allocate_ui_with_layout([ui.available_width(), 20.0].into(), egui::Layout::top_down(egui::Align::Center), |ui| {
            ui.add_space(5.0);
            ui.horizontal(|ui| {
            ui.with_layout(egui::Layout::left_to_right(egui::Align::Min), |ui|{                    
                if ui.button("⏮").clicked(){
                    self.back();
                }                    
                if ui.button( if self.playing { " ⏸ " } else { " ▶ " }).clicked() {
                self.playing = !self.playing;
                if self.playing{
                        self.play();
                    }else{
                        self.pause();
                    }
                }
                if ui.button("⏭").clicked(){
                    self.audio_sink.clear();
                }
            });

            ui.with_layout(egui::Layout::left_to_right(egui::Align::Min), |ui|{
                //slider, time , volume
                    let default_slider_width = ui.style_mut().spacing.slider_width;
                    ui.style_mut().spacing.slider_width = (ui.available_width() - 225.0).max(0.0);
                    let response = ui.add(
                        egui::Slider::new(&mut self.progress, 0.0..=1.0)
                        .show_value(false)
                        .trailing_fill(true)
                    );
                    ui.style_mut().spacing.slider_width = default_slider_width;
                    
                    if response.drag_stopped() {
                        if self.queue_indices.len() > 0 {
                            let song_len = self.song_info[self.song_current_position.unwrap()].duration.as_secs_f32();
                            self.seek_to(self.progress * song_len);
                        }else{
                            self.progress = 0.0;
                        }
                    }

                    let total_time = if self.song_current_position.is_some() {
                        self.song_info[self.song_current_position.unwrap()].duration.as_secs()
                    }else{
                        0
                    };
                    let pred_time = (self.progress * total_time as f32) as u64;
                    let time_string = std::format!("{:}:{:02} / {:}:{:02}", pred_time/60, pred_time%60, total_time/60, total_time%60);
                    ui.label(time_string);
                    
                    let volume_icon = if self.volume > 0.7 { "🔊" } else if self.volume > 0.5 { "🔉 "} else if self.volume > 0.0 {"🔈"} else { "🔇"}; 
                    let volume_string = std::format!("{:} {:.0}% ", volume_icon, self.volume * 100.0);
                    ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                        ui.add(egui::Slider::new(&mut self.volume, 0.0..=1.0).show_value(false).text(volume_string));
                        self.audio_sink.set_volume(self.volume);
                    }); 
                });
                });
                ui.add_space(5.0);
            });
        });
        
        //Library & Queue
        let mut filtered_song_indices = Vec::new();
        egui::CentralPanel::default().show(ctx, |ui| {
            //Input handling
            if ui.input(|i| i.key_pressed(egui::Key::Space)) {
                if self.playing {
                    self.pause();
                }else{
                    self.play();
                }
            }
            ui.with_layout(egui::Layout::top_down(egui::Align::Center), |ui| {
            ui.with_layout(egui::Layout::top_down_justified(egui::Align::Center), |ui| {
            
            //Searching
            ui.add(
                TextEdit::singleline(&mut self.filter_text)
                    .hint_text("Search..."),
            );
            if !self.filter_text.eq(""){
                for i in 0..self.song_info.len(){
                    if self.song_info[i].track.to_lowercase().contains(&self.filter_text.to_lowercase()){
                        filtered_song_indices.push(i);
                    }else if self.song_info[i].artist.to_lowercase().contains(&self.filter_text.to_lowercase()){
                        filtered_song_indices.push(i);
                    }else if self.song_info[i].album.to_lowercase().contains(&self.filter_text.to_lowercase()){
                        filtered_song_indices.push(i);
                    }
                }
            }else{
                for i in 0..self.song_info.len(){
                    filtered_song_indices.push(i);
                }
            }
            });

            ui.separator();

            ui.with_layout(egui::Layout::left_to_right(egui::Align::Min), |ui| {

            let library_size = if self.queue_indices.len() > 0 {
                ui.available_width() / 2.0
            } else{
                ui.available_width()
            };
            
            //Library
            ui.with_layout(egui::Layout::top_down(egui::Align::Min),|ui|{
                ui.allocate_ui([library_size, ui.available_height()].into(), |ui|{
                    ui.horizontal(|ui| {
                        ui.label("Library");
                        if ui.button("Shuffle Play").clicked(){
                            self.shuffle_play();
                        }
                    });
                    ui.separator();
                    egui::ScrollArea::vertical().auto_shrink([false, false]).id_salt("LoadedSongs").show(ui, |ui| {
                        for (album_hash, album_vec) in self.library.albums.clone().into_iter(){
                            let (album, artist) = album_hash.clone();
                            let desired_size = egui::vec2(ui.available_width() - 15.0, 24.0);
                            let ar_string = album.clone() + " - " + &artist.clone();
                            let enabled = self.enabled_album.get(&album_hash.clone()).unwrap_or(&false).clone();
                            let col_width = (ui.available_width() - 10.0) / 2.5;
                            let font_size = ui.style().text_styles.get(&egui::TextStyle::Body).map(|p| p.size).unwrap_or(14.0);
                            let approx_char_width = (font_size * 0.6).max(4.0);
                            let max_chars = (col_width / approx_char_width).floor() as usize; 
                            ui.allocate_ui(desired_size, |ui|{
                                let col_response = egui::CollapsingHeader::new("").id_salt(ar_string.as_str())
                                    .default_open(false)
                                    .open(Some(enabled))
                                    .show(ui, |ui| {
                                        for song_index in album_vec.songs {
                                            let song = self.song_info[song_index].clone();
                                            let mut number = song.track_number.unwrap_or(usize::MAX);
                                            if number == usize::MAX{
                                                number = 0;
                                            }
                                            let label = format!(
                                                "{}. {} ({:}:{:02})",
                                                number,
                                                song.track,
                                                song.duration.as_secs() / 60,
                                                song.duration.as_secs() % 60
                                            );
                                            ui.allocate_ui_with_layout(ui.available_size(), egui::Layout::centered_and_justified(egui::Direction::TopDown), |ui|{
                                                let res = ui.selectable_label(Some(song_index) == self.song_current_position, label);
                                                if res.clicked() {
                                                    self.queue_current_position = 0;
                                                    self.queue_indices = Vec::new();                                
                                                    self.add_song_to_queue_with_index(filtered_song_indices[song_index]);
                                                    self.play_immediately_with_index(filtered_song_indices[song_index]);
                                                }
                                                res.context_menu(|ui| {
                                                    if ui.button("Queue Song").clicked(){
                                                        self.add_song_to_queue_with_index(filtered_song_indices[song_index]);
                                                    }
                                                });
                                            });
                                        }
                                    });
                                let mut job = LayoutJob::default();
                                job.halign = Align::Min;
                                job.append(
                                    PlayerApp::ellipsize(ar_string.clone(), max_chars).as_str(),
                                    0.0,
                                    TextFormat {
                                        font_id: egui::FontId::proportional(14.0),
                                        color: Color32::WHITE,
                                        valign: egui::Align::Min,
                                        ..Default::default()
                                    }
                                );
                                    
                                let header_rect = col_response.header_response.rect;
                                let mut real_header_rect = header_rect;
                                real_header_rect.max.x = ui.available_width() - 15.0;
                                let real_header_res = ui.allocate_rect(real_header_rect, egui::Sense::click());
                                let text_y = real_header_rect.center().y;
                                let spacing = 20.0;
                                let left = egui::pos2(real_header_rect.left() + spacing, text_y);
                                let right = egui::pos2(real_header_rect.right() - spacing, text_y);
                                
                                if real_header_res.hovered(){
                                    ui.painter().text(left, egui::Align2::LEFT_CENTER, 
                                        PlayerApp::ellipsize(album.clone(), max_chars), 
                                        egui::TextStyle::Body.resolve(ui.style()), Color32::LIGHT_BLUE);
                                    
                                    ui.painter().text(right, egui::Align2::RIGHT_CENTER, 
                                        PlayerApp::ellipsize(artist.clone(), max_chars), 
                                        egui::TextStyle::Body.resolve(ui.style()), Color32::LIGHT_BLUE);         
                                }else{
                                    ui.painter().text(left, egui::Align2::LEFT_CENTER, 
                                        PlayerApp::ellipsize(album.clone(), max_chars), 
                                        egui::TextStyle::Body.resolve(ui.style()), Color32::WHITE);
                                    
                                    ui.painter().text(right, egui::Align2::RIGHT_CENTER, 
                                        PlayerApp::ellipsize(artist.clone(), max_chars), 
                                        egui::TextStyle::Body.resolve(ui.style()), Color32::WHITE);    
                                }
                                if real_header_res.clicked(){
                                    self.enabled_album.insert(album_hash, !enabled);
                                }
                                real_header_res.context_menu(|ui| {
                                    if ui.button("Queue Album").clicked() {
                                        if let Some(s) = self.song_info.iter().find(|s| s.album == album){
                                            self.queue_album(s.clone());
                                        }
                                    ui.close(); 
                                }
                                });                 
                            });
                        }
                    });
                });
            });

            //Queue
            if self.queue_indices.len() > 0 {
                ui.separator();
                ui.with_layout(egui::Layout::top_down(egui::Align::Min),|ui|{
                    ui.allocate_ui(ui.available_size(), |ui|{
                        ui.horizontal(|ui|{
                            ui.label("Queue");
                            if ui.button("Clear").clicked(){
                                self.clear_queue();
                            }
                            if ui.button("Shuffle Queue").clicked(){
                                self.shuffle_queue();
                            }
                        });
                        
                        ui.separator();
                        egui::ScrollArea::vertical().auto_shrink([false, true]).id_salt("QueueSongs").show(ui, |ui| {
                            let mut remove: Option<usize> = None;
                            let mut immedate_queue: Option<usize> = None;                    
                            let mut immediate_play: Option<usize> = None;
                            let inds = (self.song_current_position, self.queue_current_position);
                            let response = egui_dnd::dnd(ui, "dnd_queue")
                                .show_vec(&mut self.queue_indices, |ui, item, handle, state|{
                                handle.ui(ui, |ui|{
                                    ui.set_width(ui.available_width() - 10.0);
                                    let song_index = item.song_index;
                                    let song = self.song_info[song_index].clone();
                                    let col_width = (ui.available_width()) / 3.0;
                                    let font_size = ui.style().text_styles.get(&egui::TextStyle::Body).map(|p| p.size).unwrap_or(14.0);
                                    let approx_char_width = (font_size * 0.7).max(4.0);
                                    let max_chars = (col_width / approx_char_width).floor() as usize;
                                    let selected = state.index == self.queue_current_position;
                                    
                                    let (rect, response) = ui.allocate_exact_size([ui.available_width() - 35.0, 24.0].into(), egui::Sense::CLICK);
                                    if selected{
                                        ui.painter().rect_filled(rect, 4.0, ui.visuals().selection.bg_fill);
                                    }else{
                                        ui.painter().rect_filled(rect, 4.0, ui.visuals().widgets.inactive.bg_fill);
                                    }
                                    
                                    let spacing = 15.0;
                                    let text_y = rect.center().y;
                                    let left = egui::pos2(rect.left() + spacing, text_y);
                                    let right = egui::pos2(rect.right() - spacing, text_y);
                                
                                    if response.hovered(){
                                        ui.painter().text(left, egui::Align2::LEFT_CENTER, 
                                            PlayerApp::ellipsize(song.track.clone(), max_chars), 
                                            egui::TextStyle::Body.resolve(ui.style()), Color32::LIGHT_BLUE);
                                        
                                        ui.painter().text(right, egui::Align2::RIGHT_CENTER, 
                                            PlayerApp::ellipsize(song.artist.clone(), max_chars), 
                                            egui::TextStyle::Body.resolve(ui.style()), Color32::LIGHT_BLUE);         
                                    }else{
                                        ui.painter().text(left, egui::Align2::LEFT_CENTER, 
                                            PlayerApp::ellipsize(song.track.clone(), max_chars), 
                                            egui::TextStyle::Body.resolve(ui.style()), Color32::WHITE);
                                        
                                        ui.painter().text(right, egui::Align2::RIGHT_CENTER, 
                                            PlayerApp::ellipsize(song.artist.clone(), max_chars), 
                                            egui::TextStyle::Body.resolve(ui.style()), Color32::WHITE);    
                                    }
                                    
                                    if response.clicked() {
                                        immediate_play = Some(state.index);
                                    }
                                    response.context_menu(|ui|{
                                        if ui.button("Queue Song").clicked() {
                                            immedate_queue = Some(state.index);
                                            ui.close(); 
                                        }                                
                                        if ui.button("Remove from Queue").clicked() {
                                            remove = Some(state.index);
                                            ui.close(); 
                                        }
                                    });
                                });
                            });

                            if response.is_dragging() && self.playing {
                                let curr_playing_idx = self.queue_indices.iter().position(|x| x.song_index == inds.0.unwrap());
                                self.queue_current_position = curr_playing_idx.expect("failed to find current song in queue :SHOULDNT HAPPEN");
                            }
                            if response.is_drag_finished() {
                                response.update_vec(&mut self.queue_indices);
                            }
                            if immediate_play.is_some(){
                                self.queue_current_position = immediate_play.unwrap();
                                self.play_immediately_with_index(self.queue_indices[immediate_play.unwrap()].song_index);
                            }
                            if immedate_queue.is_some(){
                                self.add_song_to_queue_with_index(self.queue_indices[immedate_queue.unwrap()].song_index);
                            }
                            if remove.is_some(){
                                println!("REmove");
                                self.queue_indices.remove(remove.unwrap());
                                if remove.unwrap() == self.queue_current_position{
                                    println!("REmove is current position");
                                    self.audio_sink.clear();
                                    if self.queue_current_position < self.queue_indices.len() && self.queue_indices.len() > 1{
                                        println!("remove valid restart");
                                        self.play_immediately_with_index(self.queue_indices[self.queue_current_position].song_index);
                                    }
                                }
                            }
                        });
                    });
                });
            }
            });
        });
        });
        

        //Discord Integration
        if self.playing{
            //something playing
            let mut should_update = false;
            if currently_playing.is_none(){
                should_update = true;
            }else if currently_playing.unwrap() != self.song_current_position.unwrap(){
                should_update = true;
            }
            if should_update{
                    let song = self.song_info[self.song_current_position.unwrap()].clone();
                    self.discord_client.set_activity(activity::Activity::new()
                        .activity_type(activity::ActivityType::Listening)
                        .state(&song.artist.clone())
                        .details(&song.track.clone())
                        .status_display_type(activity::StatusDisplayType::State)
                        .assets(Assets::new()
                            .large_image("grad2")
                            .large_text(&song.album.clone())
                            .small_image("play")
                            .small_text("github.com/will-ixs")
                        )
                    ).expect("Failed to set activity");   
                    println!("Updated playing discord activity");
                }
        }

        //144hz refresh
        if self.playing{
            ctx.request_repaint_after(std::time::Duration::from_millis(6));
        }

    }

    fn save(&mut self, _storage: &mut dyn Storage) {
        let joined = self.dirs.iter().map(|p| p.to_string_lossy()).collect::<Vec<_>>().join(";");
        _storage.set_string("dirs", joined);
        _storage.set_string("vol", self.volume.to_string());
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        let _ = self.discord_client.close();
    }
}