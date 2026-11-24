//! The recorded CLI cases (rust/app-cases) as cargo tests: `cargo test` replays them against
//! this binary. The four cases whose output is not UTF-8 are checked by rust/bytes-check.py.
//! On Windows the cases of rust/app-cases-windows run too.

#[test]
fn app_cases() {
    let cases = trycmd::TestCases::new();
    cases
        .register_bin("nnn", trycmd::cargo::cargo_bin("nnn"))
        .case("../app-cases/*.toml");
    if cfg!(windows) {
        cases.case("../app-cases-windows/*.toml");
    }
}
