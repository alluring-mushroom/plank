use camino::Utf8Path;
use color_eyre::eyre::Result;
use ignore::Walk;
use quick_xml::de::from_str;
use serde::Deserialize;

/// a colcon `package.xml` description
#[derive(Deserialize, Debug)]
struct Package {
    name: String,
    depend: Option<Vec<String>>,
    build_depend: Option<Vec<String>>,
    exec_depend: Option<Vec<String>>,
}

/// A list of build time and runtime dependencies
#[derive(Deserialize, Debug)]
#[serde(from = "Package")]
struct Dependencies {
    build: Vec<String>,
    run: Vec<String>,
}

impl From<Package> for Dependencies {
    fn from(value: Package) -> Self {
        let mut build = value.build_depend.unwrap_or_default();
        let mut run = value.exec_depend.unwrap_or_default();
        if let Some(depend) = value.depend {
            build.extend(depend.clone());
            run.extend(depend);
        }

        Self { build, run }
    }
}

fn main() -> Result<()> {
    env_logger::init();

    for path in Walk::new("./")
        .into_iter()
        .filter_map(|e| e.ok())
        .filter_map(|p| Utf8Path::from_path(p.path()).map(Utf8Path::to_path_buf))
        .filter(|p| p.ends_with("package.xml"))
    {
        log::debug!("found package: {}", path);
        let content = std::fs::read_to_string(path)?;
        let data: Dependencies = from_str(&content)?;
        println!("{:?}", data);
    }

    Ok(())
}
