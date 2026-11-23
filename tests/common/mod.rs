#![allow(dead_code)]
use harness_cms::{store, App, Shared};
use rand::Rng;

pub fn app() -> Shared {
    let dir = std::env::temp_dir().join(format!("harness-cms-test-{:016x}", rand::thread_rng().gen::<u64>()));
    std::fs::create_dir_all(&dir).unwrap();
    App::new(
        store::open(":memory:").unwrap(),
        dir,
        "http://127.0.0.1:4200".into(),
        "测试站".into(),
    )
}
