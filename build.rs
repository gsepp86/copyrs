fn main() {
    // Incorpora l'icona nell'exe (solo Windows).
    #[cfg(windows)]
    {
        let mut res = winres::WindowsResource::new();
        res.set_icon("assets/icon.ico");
        res.set("ProductName", "CopyRs");
        res.set("FileDescription", "CopyRs - copia file con verifica");
        if let Err(e) = res.compile() {
            println!("cargo:warning=icona non incorporata: {e}");
        }
    }
}
