//! Persistence module split across logical submodules.

mod binders;
mod connection;
mod songs;

pub use binders::{create_binder, delete_binder, fetch_binders, update_binder};
pub use connection::ensure_schema;
pub use songs::{
    add_song_to_binder, create_song, delete_song, fetch_all_songs, fetch_available_songs,
    fetch_composers, fetch_songs_for_binder, remove_song_from_binder, update_song,
};
