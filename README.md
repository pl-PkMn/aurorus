# Aurorus

Aurorus is a small and a really bad AUR Helper, written in one main.rs file using Rust.

## Features

- Search for packages in AUR and repositories found in /etc/pacman.conf
- Install packages and check dependencies
- Uninstall packages and (hopefully) cleanup
- Simple CLI interface (In my opinion)

## Installation

To build and install Aurorus, you need to have Rust and Cargo installed. Clone the repository and build the project:

```sh
git clone https://github.com/yourusername/aurorus.git
cd aurorus
cargo build --release
