use nyx_client::modules::{Category, ModuleHandler};

fn main() {
    let module_handler = ModuleHandler::with_builtin_modules();

    println!(
        "NyxClient initialized with {} module(s).",
        module_handler.len()
    );
    for category in Category::ALL {
        println!(
            "{}: {}",
            category,
            module_handler.by_category(category).count()
        );
    }
}
