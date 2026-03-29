fn main() {
    #[cfg(windows)]
    {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/icon.ico");
        res.set("ProductName", "TentaFlow Desktop");
        res.set("FileDescription", "TentaFlow Desktop — Local AI with mesh networking");
        if let Err(e) = res.compile() {
            eprintln!("Warning: Failed to set Windows resources: {}", e);
        }
    }
}
