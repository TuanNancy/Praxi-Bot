//! Capture the main display with the macOS backend and write a PNG, for
//! manual checks (`cargo run -p desktop-macos --example capture-macos`).

fn main() {
    #[cfg(target_os = "macos")]
    {
        if let Err(error) = run() {
            eprintln!("capture failed: {error}");
            std::process::exit(1);
        }
    }
    #[cfg(not(target_os = "macos"))]
    println!("this example only runs on macOS");
}

#[cfg(target_os = "macos")]
fn run() -> Result<(), Box<dyn std::error::Error>> {
    use desktop_api::{Quality, Region};
    use desktop_macos::capture;

    if !capture::screen_recording_granted() {
        println!(
            "Screen Recording permission is missing: requesting it now (if no prompt \
             appears, enable it in System Settings > Privacy & Security > Screen Recording)"
        );
        let _ = capture::request_screen_recording();
    }
    let image = capture::capture(&Region::FullScreen, Quality::Lossless)?;
    let path = std::env::temp_dir().join("praxi-macos-shot.png");
    std::fs::write(&path, &image.data)?;
    println!(
        "captured {}x{} ({}) -> {}",
        image.width,
        image.height,
        image.mime_type,
        path.display()
    );
    Ok(())
}
