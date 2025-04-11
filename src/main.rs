use std::collections::{BTreeMap, HashMap, HashSet};
use std::process::Command;

use camino::Utf8Path;
use color_eyre::Section;
use color_eyre::eyre::{OptionExt, Result, eyre};
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
    color_eyre::install()?;

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

    // convert the HashMap to a BTreeMap, inverting the value and key so we can access ranges of
    // popularity. This is assumed to be fine compared to initially constructing a BTreeMap, as
    // we need to find the popularity first anyway, so reconstruction seems inevitable
    let build_popularity = {
        let mut map = BTreeMap::<u32, Vec<String>>::new();
        for (pack, pop) in build_popularity
            .into_iter()
            .filter(|e| !local_packages.contains(&e.0))
        {
            map.entry(pop).or_insert_with(|| Vec::new()).push(pack);
        }

        map
    };

    // make a single layer from popularity 4 and above inclusive
    let top_layer = build_popularity
        .range(4..)
        .into_iter()
        .map(|e| e.1.to_owned())
        .reduce(|mut acc, mut list| {
            acc.append(&mut list);
            acc
        })
        .ok_or_eyre("no popularity above 3, cannot form top_layer")?;

    // call rosdep with this data
    let result = {
        let bytes = std::process::Command::new("rosdep")
            .args(["--rosdistro", "jazzy", "resolve"])
            .args(top_layer)
            .output()
            .with_note(|| format!("Trying to call `rosdep`"))?
            .stdout;
        String::from_utf8(bytes)?
    };

    println!("{:?}", result);

    Ok(())
}
