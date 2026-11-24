fn main() {
    println!("cargo:rerun-if-changed=grandhonor.rc");
    println!("cargo:rerun-if-changed=../Assets/GrandHonor.ico");
    embed_resource::compile("grandhonor.rc", embed_resource::NONE)
        .manifest_optional()
        .expect("GrandHonor icon resource");
}
