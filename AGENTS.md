# Lave-Station

This is a first-class Rust/Linux/Gtk application that exposes Docker features for the local developer.

All business logic must have corresponding tests

Running and passing the tests is a pre-requisite for completing any task.

By preference we do Test Driven Development such that passing the test(s) *is* completing the task!

No use of unsafe is permitted.

No use of unwrap is permitted. All errors must be explicitly handled.

Use of expect is permitted only in the initial bootstrap of the application where (for example) missing config information might reasonably prevent the application from booting up.

Use the Clap tool to parse and document command line options.

Any comments in the code must be very concise.

## Tooling preferences

Prefer native Debian/Linux command-line tools (jq, grep, sed, awk, etc.) over Python, Bash, and similar scripting languages for scripting and text processing. They are more succinct for the job and always available on the target platform.

## Other best-practices docs

Consult as necessary:

  * [Container Daemon integration best practices](./docs/container_daemon_integration.md)
  * [GTK4 Applications in Rust](./docs/gtk4_applications_in_rust.md)

