# Aurorus

Aurorus is an AUR Helper, a very minimal and terrible one. All in one main.rs file. Written in Rust

## Features

- Search packages in AUR and Repositories found in /etc/pacman.conf
- Install packages and check dependency
- Uninstall packages with proper cleanup
- Act like a CLI App

## Installation

To build and install Aurorus, you need to have Rust and Cargo installed. Clone the repository and build the project:

```sh
git clone https://github.com/yourusername/aurorus.git
cd aurorus
cargo build --release