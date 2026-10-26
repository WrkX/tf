//! prompt-evals für die eingebauten presets. standardmäßig ignoriert, weil ein lauffähiges backend nötig ist. mit apple foundation models: `TF_EVAL_BACKEND=apple cargo test -p tf-core --test modes -- --ignored --nocapture`. oder gegen einen beliebigen openai-kompatiblen endpoint: `TF_EVAL_BACKEND=endpoint TF_EVAL_URL=http://127.0.0.1:11434/v1 TF_EVAL_MODEL=qwen3:4b cargo test -p tf-core --test modes -- --ignored --nocapture`.

use std::time::{Duration, Instant};

use tf_core::mode::{self, Mode};
use tf_core::prompt;
use tf_llm::{Refiner, Request};

fn backend() -> Option<Box<dyn Refiner>> {
    match std::env::var("TF_EVAL_BACKEND").as_deref() {
        Ok("apple") => tf_llm::build(tf_llm::Spec::Apple).ok(),
        Ok("endpoint") => {
            let url = std::env::var("TF_EVAL_URL").ok()?;
            let model = std::env::var("TF_EVAL_MODEL").ok()?;
            tf_llm::build(tf_llm::Spec::Endpoint(tf_llm::Endpoint {
                base_url: url,
                model,
                api_key: std::env::var("TF_EVAL_KEY").ok(),
            }))
            .ok()
        }
        _ => None,
    }
}

fn clean(mode: &Mode, input: &str, backend: &mut dyn Refiner) -> Result<String, String> {
    let system = prompt::system_prompt(mode, &[]);
    let user = prompt::user_prompt(input);
    let request = Request::new(&system, &user, 0.2, prompt::max_tokens(input), Duration::from_secs(30));
    let output = backend.refine(&request).map_err(|e| e.reason())?;
    Ok(prompt::clean_output(&output, input))
}

#[test]
#[ignore = "needs a local or Apple LLM backend; see the module docs"]
fn presets_do_not_answer_or_drop_language() {
    let Some(mut backend) = backend() else {
        eprintln!("skipped: set TF_EVAL_BACKEND to apple or endpoint");
        return;
    };
    let clean_mode = mode::preset("clean").unwrap();
    let cases: &[(&str, &str, bool)] = &[
        ("what's the capital of France", "capital of France", false),
        ("um so I think we should uh meet on Tuesday no wait Thursday", "Thursday", true),
        ("ähm ich denke wir treffen uns am Dienstag", "wir", false),
    ];
    let mut passed = 0;
    let mut latencies = Vec::new();
    for (input, needle, forbid_filler) in cases {
        let started = Instant::now();
        let output = clean(&clean_mode, input, backend.as_mut()).unwrap_or_else(|e| panic!("{e}"));
        latencies.push(started.elapsed().as_millis() as u64);
        let lowered = output.to_lowercase();
        let ok = lowered.contains(&needle.to_lowercase())
            && (!forbid_filler || (!lowered.contains("um ") && !lowered.contains("uh ")))
            && !lowered.contains("paris");
        println!("{}\n  → {}  {}", input, output, if ok { "pass" } else { "FAIL" });
        passed += usize::from(ok);
    }
    let average = latencies.iter().sum::<u64>() / latencies.len().max(1) as u64;
    println!("{passed}/{} passed, average {} ms", cases.len(), average);
    assert_eq!(passed, cases.len());
}
