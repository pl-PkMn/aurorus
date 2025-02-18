# aurorus

aurorus is a very basic and terrible AUR Helper. Written in Rust and in single main.rs file.

## Installation
   ```sh
   git clone https://aur.archlinux.org/aurorus.git
   cd aurorus
   makepkg -si
   ```
   
## Usage
Runnning aurorus :
```sh
aurorus
```
Commands :

search, s <package> : Search for a package in the AUR (sorted by votes) and repositories in '/etc/pacman.conf'.

install, i <package> : Install a package from the AUR or repositories in '/etc/pacman.conf'.

uninstall, ui <package> : Uninstall a package.

help : Show help message.

exit : Exit the application.

### Examples

- **Search for a package:**
  ```sh
  aurorus search <package_name>
  ```

- **Install a package:**
  ```sh
  aurorus install <package_name>
  ```

## Contributing

Feel free to open issues or submit pull requests if you have any improvements or bug fixes. (Though, chances are, I'll probably leave it.)

## License

This project is licensed under the MIT License. See the [LICENSE](LICENSE) file for details.
