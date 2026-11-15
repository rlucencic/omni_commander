fn main() {
    // Use the same widget style on every platform; the Fluent style follows
    // the system light/dark preference.
    let config = slint_build::CompilerConfiguration::new().with_style("fluent".into());
    slint_build::compile_with_config("ui/app.slint", config).expect("Slint build failed");
}
