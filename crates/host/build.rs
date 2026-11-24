fn main() {
    println!("cargo:rerun-if-changed=grandhonor.rc");
    println!("cargo:rerun-if-changed=grandhonor.manifest");
    println!("cargo:rerun-if-changed=../../Assets/GrandHonor.ico");
    println!("cargo:rerun-if-env-changed=GRANDHONOR_PANEL_HASH");
    if let Ok(hash) = std::env::var("GRANDHONOR_PANEL_HASH") {
        println!("cargo:rustc-env=GRANDHONOR_PANEL_HASH={hash}");
    }
    embed_resource::compile("grandhonor.rc", embed_resource::NONE)
        .manifest_required()
        .expect("GrandHonor resource compilation");
}
