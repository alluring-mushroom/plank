use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::LazyLock;

use camino::{Utf8Path, Utf8PathBuf};
use clap::Parser;
use color_eyre::Section;
use color_eyre::eyre::{Result, eyre};
use ignore::Walk;
use quick_xml::de::from_str;
use regex_lite::{Captures, Regex};
use serde::Deserialize;

/// a colcon `package.xml` description
#[derive(Deserialize, Debug, Clone)]
struct ColconPackage {
    name: String,
    depend: Option<Vec<String>>,
    build_depend: Option<Vec<String>>,
    exec_depend: Option<Vec<String>>,
}

type Packages = HashMap<Name, Package>;
type Name = String;

#[derive(Debug)]
struct Package {
    path: Utf8PathBuf,
    build: Vec<String>,
    run: Vec<String>,
}

impl Package {
    fn from_colcon_package(path: Utf8PathBuf, colcon_package: ColconPackage) -> Self {
        let mut build = colcon_package.build_depend.unwrap_or_default();
        let mut run = colcon_package.exec_depend.unwrap_or_default();
        if let Some(depend) = colcon_package.depend {
            build.extend(depend.clone());
            run.extend(depend);
        }

        Self { path, build, run }
    }
}

#[derive(Debug, Eq, PartialEq)]
struct Layer {
    name: String,
    system_dependencies: Dependencies,
    local_dependencies: HashSet<String>,
}

/// ensure correct ordering of layers such that they respect Docker rules
impl PartialOrd for Layer {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for Layer {
    fn cmp(&self, other: &Self) -> Ordering {
        if other.local_dependencies.contains(&self.name) {
            Ordering::Less
        } else if self.local_dependencies.contains(&other.name) {
            Ordering::Greater
        } else {
            self.name.cmp(&other.name)
        }
    }
}

/// A dependency can either be a name as it is listed in the package, or the full system name as
/// resolved by rosdep
#[derive(Eq, PartialEq, Debug)]
enum Dependencies {
    None,
    Raw(HashSet<String>),
    Resolved(String),
}

/// resolves packages names to system names
fn resolve_packages(resolver: &str, args: &HashSet<String>) -> Result<String> {
    // use regex to replace each `{}` with `args`
    static RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(.)?\{\}").unwrap());
    let replacement = &args
        .iter()
        .map(String::as_str)
        .collect::<Vec<&str>>()
        .join(" ");
    let resolved = RE.replace_all(resolver, |captures: &Captures| match &captures.get(1) {
        Some(v) if v.as_str() == "#" => "{}".to_string(),
        Some(v) if v.as_str() == r"\" => "{}".to_string(),
        other => format!("{}{}", other.map(|v| v.as_str()).unwrap_or(""), replacement),
    });

    Ok(resolved.into_owned())
}

#[derive(Parser)]
#[command(version, about, long_about = None)]
struct Cli {
    /// Path to search for packages. Defaults to CWD
    path: Option<String>,

    /// the minimum popularity a package needs to be in the top layer. Defaults to 4
    #[arg(short = 'p', long)]
    min_popularity: Option<u32>,

    /// Command to convert a dependency name to an action, such as apt installing
    /// Any occurance of `{}` will be replaced with the dependencies for a single package
    /// use either `\` or `#` to escape this, eg. `echo \{}` will result in `echo {}`
    #[arg(short, long)]
    resolver: String,

    /// dependencies to ignore if they are seen
    #[arg(long)]
    ignore: Vec<String>,
}

fn main() -> Result<()> {
    env_logger::init();
    color_eyre::install()?;

    let cli = Cli::parse();
    let target_path = cli.path.unwrap_or("./".to_string());
    let min_popularity = cli.min_popularity.unwrap_or(4);
    let resolver = cli.resolver.as_str();
    let ignore: HashSet<String> = cli.ignore.into_iter().collect();

    // construct map of dependencies to popularity of the dependency

    let mut build_popularity = HashMap::<Name, u32>::new();
    // local packages don't need to be installed, so track them
    let mut local_packages = Packages::new();

    for path in Walk::new(target_path)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter_map(|p| Utf8Path::from_path(p.path()).map(Utf8Path::to_path_buf))
        .filter(|p| p.ends_with("package.xml"))
    {
        log::debug!("found package: {}", path);

        let content = std::fs::read_to_string(&path)?;
        let data: ColconPackage = from_str(&content)?;
        let name = data.name.clone();
        let package = Package::from_colcon_package(path, data);

        for build_dependency in package.build.iter() {
            build_popularity
                .entry(build_dependency.to_owned())
                .and_modify(|e| *e += 1)
                .or_insert(1);
        }

        local_packages.insert(name, package);
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
        .range(min_popularity..)
        .into_iter()
        .map(|e| e.1.to_owned())
        .reduce(|mut acc, mut list| {
            acc.append(&mut list);
            acc
        })
        .ok_or_else(|| eyre!("no popularity >= {}, cannot form top_layer", min_popularity))?
        .into_iter()
        .collect();

    // loop through packages to create layers, which requires separating local dependencies
    // (packages that are on this system) and system dependencies, which will be resolved using a
    // resolver
    let mut layers = BTreeSet::new();
    for (name, package) in &local_packages {
        let mut system_dependencies = HashSet::new();
        let mut local_dependencies = HashSet::new();

        for dependency in &package.build {
            if local_packages.contains_key(dependency.as_str()) {
                local_dependencies.insert(dependency.to_owned());
            } else if !top_layer.contains(dependency.as_str()) {
                system_dependencies.insert(dependency.to_owned());
            }
        }
        layers.insert(Layer {
            name: name.to_owned(),
            system_dependencies: if system_dependencies.len() > 0 {
                Dependencies::Raw(system_dependencies)
            } else {
                Dependencies::None
            },
            local_dependencies,
        });
    }

    // replace the list of dependencies with the resolver, a command to run that will install those
    // dependencies
    let layers = {
        let mut new_layers = BTreeSet::new();
        for mut layer in layers {
            if let Dependencies::Raw(ref dependencies) = layer.system_dependencies {
                // exclude dependencies that are ignored by the user and in the top layer
                let dependencies: HashSet<String> = dependencies - &(&ignore - &top_layer);
                if dependencies.len() > 0 {
                    let resolved = resolve_packages(resolver, &dependencies)
                        .with_note(|| format!("parsing {}", layer.name))?;
                    layer.system_dependencies = Dependencies::Resolved(resolved);
                } else {
                    layer.system_dependencies = Dependencies::None;
                }
            }
            new_layers.insert(layer);
        }

        new_layers
    };

    println!("{:?}", layers);

    Ok(())
}
