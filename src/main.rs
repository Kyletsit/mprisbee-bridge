use std::io::BufRead;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixListener;
use std::fs::{create_dir, set_permissions, Permissions};
use users::get_current_uid;


fn main() {
    // Create dir and remove socket if there
    let path = format!("/tmp/mprisbee{}/", get_current_uid());
    let path = path.as_str();
    eprintln!("D: dir {}", path);
    create_dir(path).unwrap_or_else(|e| match e.kind() {
        std::io::ErrorKind::AlreadyExists => (),
        _ => panic!("{}", e),
    });
    set_permissions(path, Permissions::from_mode(0o700)).unwrap();

    let path = format!("{}wine-out", path);
    let path = path.as_str();
    eprintln!("D: socket {}", path);
    std::fs::remove_file(path).unwrap_or_else(|e| match e.kind() {
        std::io::ErrorKind::NotFound => (),
        _ => panic!("{}", e),
    });

    let listener = UnixListener::bind(path).unwrap();
    for stream in listener.incoming() {
        let stream = stream.unwrap();
        let reader = stream.try_clone().unwrap();
        let reader = std::io::BufReader::new(reader);

        for request in reader.split(0) {
            let bytes = request.unwrap();
            let request = String::from_utf8_lossy(&bytes);
            eprintln!("message: {}", request);

            let response = match request.trim() {
                "title: illegal paradise" => "A: marry me",
                "artist: weezer" => "A: weezo weezo weezo",
                _ => "A: thats crazy",
            };

            println!("{}", response);
        }
    }

}
