"""Run Pico's synchronous entry with observable guard and executor substitutes."""
from pathlib import Path
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]


class PicoStartup(unittest.TestCase):
    def test_guard_precedes_executor_and_future_construction_and_failure_halts(self):
        source = (ROOT / "platforms/pico/src/main.rs").read_text()
        entry = source[source.index("#[cortex_m_rt::entry]"):
                       source.index("#[embassy_executor::task]\nasync fn main_task")]
        # Run the production functions without the embedded entry-point attribute.
        entry = entry.replace("#[cortex_m_rt::entry]", "")
        fixture = r'''
#[derive(Clone, Copy)]
pub struct Spawner;
impl Spawner {
    pub fn spawn(self, _: ()) { println!("spawn"); }
}
fn main_task(_: Spawner) -> Result<(), ()> {
    println!("future");
    Ok(())
}
fn fatal() -> ! { std::process::exit(42); }
mod embassy_rp {
    pub fn install_core0_stack_guard() -> Result<(), ()> {
        println!("guard");
        if std::env::args().nth(1).as_deref() == Some("fail") { Err(()) } else { Ok(()) }
    }
    pub mod executor {
        pub struct Executor;
        impl Executor {
            pub fn new() -> Self { println!("executor"); Self }
            pub fn run(&'static mut self, init: impl FnOnce(crate::Spawner)) -> ! {
                init(crate::Spawner);
                std::process::exit(0);
            }
        }
    }
}
mod static_cell {
    pub struct StaticCell<T>(std::marker::PhantomData<T>);
    impl<T> StaticCell<T> {
        pub const fn new() -> Self { Self(std::marker::PhantomData) }
        pub fn init(&'static self, value: T) -> &'static mut T {
            println!("cell");
            Box::leak(Box::new(value))
        }
    }
}
mod startup {
    use crate::{embassy_rp, fatal, main_task, static_cell};
''' + entry + r'''
    pub fn run() -> ! { main() }
}
fn main() { startup::run() }
'''
        with tempfile.TemporaryDirectory() as directory:
            rust, binary = Path(directory, "startup.rs"), Path(directory, "startup")
            rust.write_text(fixture)
            subprocess.run(["rustc", "--edition=2024", "-O", str(rust), "-o", str(binary)],
                           check=True)
            for mode, code, events in (
                    ("ok", 0, ["guard", "executor", "cell", "future", "spawn"]),
                    ("fail", 42, ["guard"])):
                with self.subTest(mode=mode):
                    result = subprocess.run([str(binary), mode], capture_output=True, text=True,
                                            timeout=10)
                    self.assertEqual(result.returncode, code, result.stderr)
                    self.assertEqual(result.stdout.splitlines(), events)


if __name__ == "__main__":
    unittest.main()
