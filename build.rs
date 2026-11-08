fn main() {
    println!("cargo:rerun-if-changed=assets/app.rc");
    println!("cargo:rerun-if-changed=assets/destello.manifest");
    println!("cargo:rerun-if-changed=assets/destello.ico");

    #[cfg(windows)]
    {
        // La versión del .exe (Propiedades > Detalles) sale de Cargo.toml: así un release que
        // solo sube la versión en Cargo.toml no deja el recurso desfasado.
        let version = std::env::var("CARGO_PKG_VERSION").expect("CARGO_PKG_VERSION");
        let mut parts: Vec<u16> = version
            .split(['.', '-', '+'])
            .take(3)
            .map(|p| p.parse().unwrap_or(0))
            .collect();
        parts.resize(3, 0);
        let comma = format!("{},{},{},0", parts[0], parts[1], parts[2]);
        let dot = format!("{}.{}.{}.0", parts[0], parts[1], parts[2]);

        let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR");
        let assets = std::path::Path::new(&manifest_dir)
            .join("assets")
            .to_string_lossy()
            .replace('\\', "/");
        let template = std::fs::read_to_string("assets/app.rc").expect("assets/app.rc");
        let rc = template
            .replace("@VER_COMMA@", &comma)
            .replace("@VER_DOT@", &dot)
            .replace("@ASSETS@", &assets);

        let out = std::path::Path::new(&std::env::var("OUT_DIR").expect("OUT_DIR")).join("app.rc");
        std::fs::write(&out, rc).expect("escribir app.rc");
        embed_resource::compile(&out, embed_resource::NONE)
            .manifest_required()
            .expect("compilar recursos");
    }
}
