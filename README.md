# Aurorus

Aurorus is a small and a really bad AUR Helper, written in one main.rs file using Rust.

## Features

- Search for packages in AUR and repositories found in /etc/pacman.conf
- Install packages and check dependencies
- Uninstall packages and (hopefully) cleanup
- Simple CLI interface (In my opinion)

## Dependencies
Rust 1:1.84.1-1 (Higher or Lower will probably work)

## Installation
```sh
git clone https://github.com/yourusername/aurorus.git
cd aurorus
cargo build --release
```

##Usage
Run the application :
```sh
./target/release/aurorus
```
  Commands
- search <package> or s <package>: Search for packages
- install <package> or i <package>: Install a package
- uninstall <package> or ui <package>: Uninstall a package
- help: Show help message
- exit: Exit application

## License

This project is licensed under the MIT License - see the [LICENSE](LICENSE) file for details.
