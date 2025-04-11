use std::collections::{HashMap, HashSet};

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
    package: String,
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

        Self {
            package: value.name,
            build,
            run,
        }
    }
}

fn main() -> Result<()> {
    env_logger::init();

    // construct map of dependencies to popularity of the dependency

    let mut build_popularity = HashMap::<String, u32>::new();
    // local packages don't need to be installed, so track them
    let mut local_packages = HashSet::<String>::new();

    for path in Walk::new("./")
        .into_iter()
        .filter_map(|e| e.ok())
        .filter_map(|p| Utf8Path::from_path(p.path()).map(Utf8Path::to_path_buf))
        .filter(|p| p.ends_with("package.xml"))
    {
        log::debug!("found package: {}", path);

        let content = std::fs::read_to_string(path)?;
        let data: Dependencies = from_str(&content)?;

        local_packages.insert(data.package);

        for build_dependency in data.build {
            build_popularity
                .entry(build_dependency)
                .and_modify(|e| *e += 1)
                .or_insert(1);
        }
    }

    // remove local packages from popularity list and collect
    let mut popularity: Vec<(String, u32)> = build_popularity
        .into_iter()
        .filter(|(pack, _pop)| !local_packages.contains(pack))
        .collect();

    // sort first by popularity then by name
    popularity.sort_by(|a, b| match b.1.cmp(&a.1) {
        std::cmp::Ordering::Equal => a.0.cmp(&b.0),
        other => other,
    });

    for (dependency, pop) in popularity {
        println!("{}: {}", dependency, pop);
    }

    Ok(())
}
