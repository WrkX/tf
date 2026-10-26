use tf_apple::foundation_models::{self, Availability};

fn main() {
    let availability = foundation_models::availability();
    println!("Foundation Models: {availability:?}");
    if availability != Availability::Available {
        return;
    }
    let raw = "um so I think we should uh meet on tuesday no wait thursday at like three";
    match foundation_models::respond(
        "Clean up dictated text. Remove filler words and self-corrections. Output only the cleaned text.",
        raw,
    ) {
        Ok(text) => println!("{raw}\n→ {text}"),
        Err(e) => eprintln!("error: {e}"),
    }
}
