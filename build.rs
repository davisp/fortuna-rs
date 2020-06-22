use std::env;
use std::fs;
use std::fs::read_dir;
use std::path::Path;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    create_js_src_file()?;
    tonic_build::compile_protos("proto/ateles.proto")?;
    Ok(())
}

// Load all the files from js/ and create a string with them to be added to the
// snapshot isolate
fn create_js_src_file() -> Result<(), Box<dyn std::error::Error>> {
    let scripts = read_dir("./js")?
        .filter(|entry| entry.as_ref().unwrap().path().is_file())
        .map(|file| {
            let name = file.unwrap().path();
            println!("Reading {:?}", name);
            fs::read_to_string(&name).unwrap()
        })
        .collect::<Vec<String>>()
        .join("\n\n");

    let js = format!("pub const JS_CODE: &str = r#\n\"{}\"\n#;", scripts);

    let out_dir = env::var_os("OUT_DIR").unwrap();
    let dest = Path::new(&out_dir).join("js_code.rs");
    fs::write(dest, js)?;

    Ok(())
}
