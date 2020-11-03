#[derive(Debug)]
struct Metadata {
    mod_time: std::time::SystemTime,
    size: isize,
}

impl PartialEq for Metadata {
    fn eq(&self, other: &Metadata) -> bool {
        self.size == other.size && self.mod_time == other.mod_time
    }
}

impl Metadata {
    fn from(metadata: &std::fs::Metadata) -> Metadata {
        Metadata { mod_time: metadata.modified().unwrap(), size: metadata.len() as isize }
    }
}

#[derive(Debug)]
pub struct File {
    path: std::path::PathBuf,
    metadata: Option<Metadata>,
}

impl File {
    pub fn new(path: &std::path::Path) -> File {
        File { path: path.to_path_buf(), metadata: None }
    }

    pub fn read_update(&mut self) -> Option<String> {
        match std::fs::metadata(&self.path) {
            Ok(ref metadata) if metadata.is_file() => {
                let metadata = Metadata::from(&metadata);
                match self.metadata {
                    Some(ref stored_metadata) if stored_metadata == &metadata => {},
                    _ => match std::fs::read_to_string(&self.path) {
                        Ok(string) => {
                            eprintln!("Updated {:?}", &self.path);
                            self.metadata = Some(metadata);
                            return Some(string);
                        },
                        Err(err) => {
                            eprintln!("Error reading file '{:?}': '{}'", &self.path, err);
                        },
                    },
                }
            },
            _ => {},
        }

        None
    }
}
