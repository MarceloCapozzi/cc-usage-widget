fn main() {
    // tauri-build embeds icons/icon.ico in the executable, but its own
    // rerun-if-changed list does not cover the icons folder: a new icon was
    // silently left out of the build (the exe kept the old one). Listing the
    // folder makes any change there re-run this script.
    println!("cargo:rerun-if-changed=icons");
    tauri_build::build()
}
