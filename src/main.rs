use std::collections::{BTreeMap, HashMap, HashSet};
use std::process::Command;

use camino::{Utf8Path, Utf8PathBuf};
use color_eyre::Section;
use color_eyre::eyre::{OptionExt, Result, eyre};
use ignore::Walk;
use quick_xml::de::from_str;
use regex_lite::Regex;
use serde::Deserialize;

/// a colcon `package.xml` description
#[derive(Deserialize, Debug, Clone)]
struct ColconPackage {
    name: String,
    depend: Option<Vec<String>>,
    build_depend: Option<Vec<String>>,
    exec_depend: Option<Vec<String>>,
}

#[derive(Debug)]
struct Package {
    path: Utf8PathBuf,
    dependencies: Dependencies,
}

/// A list of build time and runtime dependencies
#[derive(Deserialize, Debug)]
struct Dependencies {
    build: Vec<String>,
    run: Vec<String>,
}

impl From<ColconPackage> for Dependencies {
    fn from(value: ColconPackage) -> Self {
        let mut build = value.build_depend.unwrap_or_default();
        let mut run = value.exec_depend.unwrap_or_default();
        if let Some(depend) = value.depend {
            build.extend(depend.clone());
            run.extend(depend);
        }

        Self { build, run }
    }
}

/// resolves packages names to system names
fn resolve_packages(args: HashSet<String>) -> Result<HashSet<String>> {
    let rosdep = Command::new("rosdep")
        .args(["--rosdistro", "jazzy", "resolve"])
        .args(args)
        .output()
        .with_note(|| format!("Trying to call `rosdep`"))?;

    if rosdep.stderr.len() > 0 {
        return Err(eyre!(String::from_utf8(rosdep.stderr)?));
    };

    let output = String::from_utf8(rosdep.stdout)?;

    // parse result of this command line
    // TODO: stop depending on external rosdep so this grossness isn't necessary
    let mut apt_packages = HashSet::new();
    let apt_re = Regex::new("#apt\n(.*)\n")?;

    for (_, [package]) in apt_re.captures_iter(output.as_str()).map(|c| c.extract()) {
        apt_packages.insert(package.to_string());
    }

    Ok(apt_packages)
}

fn main() -> Result<()> {
    env_logger::init();
    color_eyre::install()?;

    // construct map of dependencies to popularity of the dependency

    let mut build_popularity = HashMap::<String, u32>::new();
    // local packages don't need to be installed, so track them
    let mut local_packages = HashMap::<String, Package>::new();

    for path in Walk::new("./")
        .into_iter()
        .filter_map(|e| e.ok())
        .filter_map(|p| Utf8Path::from_path(p.path()).map(Utf8Path::to_path_buf))
        .filter(|p| p.ends_with("package.xml"))
    {
        log::debug!("found package: {}", path);

        let content = std::fs::read_to_string(&path)?;
        let data: ColconPackage = from_str(&content)?;
        let package = Package {
            path,
            dependencies: data.clone().into(),
        };

        for build_dependency in package.dependencies.build.iter() {
            build_popularity
                .entry(build_dependency.to_owned())
                .and_modify(|e| *e += 1)
                .or_insert(1);
        }

        local_packages.insert(data.name, package);
    }

    // convert the HashMap to a BTreeMap, inverting the value and key so we can access ranges of
    // popularity. This is assumed to be fine compared to initially constructing a BTreeMap, as
    // we need to find the popularity first anyway, so reconstruction seems inevitable
    let build_popularity = {
        let mut map = BTreeMap::<u32, Vec<String>>::new();
        for (pack, pop) in build_popularity
            .into_iter()
            .filter(|e| !local_packages.contains_key(&e.0))
        {
            map.entry(pop).or_insert_with(|| Vec::new()).push(pack);
        }

        map
    };

    // make a single layer from popularity 4 and above inclusive
    let top_layer: HashSet<String> = build_popularity
        .range(4..)
        .into_iter()
        .map(|e| e.1.to_owned())
        .reduce(|mut acc, mut list| {
            acc.append(&mut list);
            acc
        })
        .ok_or_eyre("no popularity above 3, cannot form top_layer")?
        .into_iter()
        .collect();

    let resolved_top_layer = resolve_packages(top_layer)?;

    println!("{:?}", resolved_top_layer);

    Ok(())
}
