//! The recorded CLI cases (rust/app-cases) as cargo tests: `cargo test` replays them against
//! this binary. The four cases whose output is not UTF-8 are checked by rust/bytes-check.py.

#[test]
fn app_cases() {
    trycmd::TestCases::new()
        .register_bin("nnn", trycmd::cargo::cargo_bin("nnn"))
        .case("../app-cases/*.toml");
}
