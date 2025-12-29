use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs::{self, File};
use std::io::Write;
use std::sync::LazyLock;

use atomic_write_file::AtomicWriteFile;
use camino::{Utf8Path, Utf8PathBuf};
use clap::Parser;
use color_eyre::Section;
use color_eyre::eyre::{Result, WrapErr, eyre};
use env_logger::Env;
use ignore::Walk;
use petgraph::{
    Directed,
    graphmap::GraphMap,
    visit::{Topo, Walker},
};
use quick_xml::de::from_str as from_xml_str;
use regex_lite::{Captures, Regex};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug)]
struct Plankconfig {
    pub top_layer: BTreeSet<String>,
}

/// a colcon `package.xml` description
#[derive(Deserialize, Debug, Clone)]
struct ColconPackage {
    name: String,
    depend: Option<Vec<String>>,
    build_depend: Option<Vec<String>>,
    exec_depend: Option<Vec<String>>,
}

type Packages = BTreeMap<Name, Package>;
type Layers = BTreeMap<Name, Layer>;
/// records how often a package is a dependency
type PackagePopularity = BTreeMap<Name, u32>;
type Name = String;

#[derive(Debug)]
struct Package {
    path: Utf8PathBuf,
    build: Vec<Name>,
    exec: Vec<Name>,
}

impl Package {
    fn from_colcon_package(path: Utf8PathBuf, colcon_package: ColconPackage) -> Self {
        let mut build = colcon_package.build_depend.unwrap_or_default();
        let mut exec = colcon_package.exec_depend.unwrap_or_default();
        if let Some(depend) = colcon_package.depend {
            build.extend(depend.clone());
            exec.extend(depend);
        }

        Self { path, build, exec }
    }
}

#[derive(Debug, Eq, PartialEq)]
struct Layer {
    name: Name,
    source: Source,
    system_dependencies: Dependencies,
    local_dependencies: BTreeSet<Name>,
}

/// ensure correct ordering of layers such that they respect Docker rules
impl PartialOrd for Layer {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for Layer {
    fn cmp(&self, other: &Self) -> Ordering {
        self.name.cmp(&other.name)
    }
}

/// a layer either depends on a path, because it uses something from the file system, or a
/// previous layer
#[derive(Debug, Eq, PartialEq)]
enum Source {
    Path(Utf8PathBuf),
    LayerName(String),
}

/// a layer has either no system dependencies or they are docker `run` command constructed to
/// install those dependencies
#[derive(Eq, PartialEq, Debug)]
enum Dependencies {
    None,
    Resolved(Vec<Name>),
}

/// resolves templated commands
fn resolve_commands<'a, I, T>(resolver: &str, args: I) -> Result<String>
where
    I: std::iter::IntoIterator<Item = T>,
    T: AsRef<str>,
{
    // use regex to replace each `{}` with `args`
    static RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(.)?\{\}").unwrap());
    let args: Vec<T> = args.into_iter().collect();
    let replacement = &args
        .iter()
        .map(AsRef::as_ref)
        .collect::<Vec<&str>>()
        .join(" ");
    let resolved = RE.replace_all(resolver, |captures: &Captures| match &captures.get(1) {
        Some(v) if v.as_str() == "#" => "{}".to_string(),
        Some(v) if v.as_str() == r"\" => "{}".to_string(),
        other => format!("{}{}", other.map(|v| v.as_str()).unwrap_or(""), replacement),
    });

    Ok(resolved.into_owned())
}

/// Create a Layer from a Package, which requires separating local dependencies
/// (packages that are on this system) and system dependencies, which will be resolved using a
/// resolver, whilst ignoring user specified packages
fn generate_layer(
    layer_name: String,
    package: Name,
    source: Source,
    dependencies: &[Name],
    local_packages: &Packages,
    ignore: BTreeSet<Name>,
    package_resolvers: &HashMap<&str, &str>,
    default_resolver: &str,
) -> Result<Layer> {
    log::debug!("Creating layer for `{package}` named `{layer_name}`");

    let (system_dependencies, local_dependencies) = {
        let mut system_dependencies = BTreeSet::new();
        let mut local_dependencies = BTreeSet::new();
        for dependency in dependencies {
            if local_packages.contains_key(dependency.as_str()) {
                local_dependencies.insert(dependency.to_owned());
            } else if !ignore.contains(dependency.as_str()) {
                system_dependencies.insert(dependency.to_owned());
            }
        }

        (system_dependencies, local_dependencies)
    };

    // replace the list of system dependencies with the resolver, a command to run that will install those
    // dependencies. Some dependencies have a specific resolver just for them, the rest use the
    // default resolver
    let system_dependencies = {
        if system_dependencies.len() > 0 {
            let mut resolved = Vec::new();
            let mut remaining = BTreeSet::new();
            for dependency in system_dependencies {
                if let Some(&command) = package_resolvers.get(dependency.as_str()) {
                    if command.is_empty() {
                        continue;
                    };
                    resolved.push(resolve_commands(command, std::iter::once(dependency))?);
                } else {
                    remaining.insert(dependency.to_owned());
                }
            }
            resolved.push(
                resolve_commands(default_resolver, &remaining)
                    .with_note(|| format!("parsing {}", &layer_name))?,
            );
            Dependencies::Resolved(resolved)
        } else {
            Dependencies::None
        }
    };
    log::debug!("resolve layer for {}", &layer_name);

    let layer = Layer {
        name: layer_name,
        source,
        system_dependencies,
        local_dependencies,
    };

    Ok(layer)
}

#[derive(Parser)]
#[command(version, about, long_about = None)]
struct Cli {
    /// Path to search for packages. Defaults to CWD
    path: Option<String>,

    /// Embed the contents of another Dockerfile in this one. It will be as if they are
    /// concatenated, with the options specified here coming before the content this program
    /// generates. May be specified more than once
    #[arg(long)]
    include: Vec<String>,

    /// location in each layer that build artifacts are stored. This is needed so that dependent
    /// code can be copied to the next layer
    #[arg(long)]
    artifact_dir: Option<String>,

    /// the base image that each layer will use
    #[arg(long)]
    base: String,

    /// the location to write the output to
    #[arg(long)]
    output: Option<String>,

    /// the minimum popularity a package needs to be in the build top layer. Defaults to 4
    #[arg(short = 'p', long)]
    min_build_popularity: Option<u32>,

    /// the minimum popularity a package needs to be in the exec top layer. Defaults to 4
    #[arg(short = 'p', long)]
    min_exec_popularity: Option<u32>,

    /// Command to convert a dependency name to an action, such as apt installing
    /// Any occurrence of `{}` will be replaced with the dependencies for a single package
    /// use either `\` or `#` to escape this, eg. `echo \{}` will result in `echo {}`
    #[arg(short = 'r', long)]
    default_resolver: String,

    /// a command to resolve a single package. It is of the form `regex:command`. If command is
    /// a blank string, the package is simply not resolved, if it is non-empty it is treated
    /// the same as `default_resolver`, but for this specific package, including substitutions. See
    /// `default_resolver` for more information
    #[arg(long)]
    package: Vec<String>,

    /// The command used to build the package.
    /// Any occurrence of `{}` will be replaced with the path, in Docker, of the package to be built
    #[arg(long)]
    build_command: String,

    /// The command that is used to specify the entrypoint of an exec layer
    /// Any occurrence of `{}` will be replaced with the name of the package that the exec layer is
    /// based on
    #[arg(long)]
    exec_command: String,

    /// dependencies to ignore if they are seen
    #[arg(long)]
    ignore: Vec<String>,

    /// whether to overwrite the top_layer of the dockerimage
    #[arg(long)]
    overwrite_top_layer: bool,
}

fn main() -> Result<()> {
    env_logger::Builder::from_env(Env::default().default_filter_or("info")).init();
    color_eyre::install()?;

    // get cli args from user
    let cli = Cli::parse();
    let target_path = cli.path.unwrap_or("./".to_string());
    let output_path = Utf8PathBuf::from(cli.output.unwrap_or("Dockerfile".to_string()));
    let include_dockerfiles = cli.include.into_iter().map(Utf8PathBuf::from);
    let artifact_dir = cli.artifact_dir.unwrap_or("".to_string());
    let artifact_dir = artifact_dir.as_str();
    let base_image = cli.base.as_str();
    let min_build_popularity = cli.min_build_popularity.unwrap_or(4);
    let min_exec_popularity = cli.min_exec_popularity.unwrap_or(4);
    let default_resolver = cli.default_resolver.as_str();
    let package_resolvers = {
        let resolvers: Result<HashMap<&str, &str>, &str> = cli
            .package
            .iter()
            .map(|s| s.split_once(":").ok_or(s.as_str()))
            .collect();
        resolvers.map_err(|e| eyre!("Couldn't process a --package argument: '{}'", e))?
    };
    let build_command = cli.build_command.as_str();
    let exec_command = cli.exec_command.as_str();
    let ignore: BTreeSet<Name> = cli.ignore.into_iter().collect();
    let overwrite_top_layer = cli.overwrite_top_layer;

    // construct map of dependencies to popularity of the dependency
    let mut build_popularity = PackagePopularity::new();
    let mut exec_popularity = PackagePopularity::new();

    let mut local_packages = Packages::new();

    for path in Walk::new(&target_path)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter_map(|p| Utf8Path::from_path(p.path()).map(Utf8Path::to_path_buf))
        .filter(|p| p.ends_with("package.xml"))
    {
        log::debug!("found package: {}", path);

        let content = fs::read_to_string(&path)?;
        let data: ColconPackage = from_xml_str(&content)?;
        let name = data.name.clone();
        let package = Package::from_colcon_package(
            path.strip_prefix(&target_path)?
                .parent()
                .ok_or_else(|| eyre!("Couldn't find parent of {}", target_path))?
                .to_path_buf(),
            data,
        );

        for build_dependency in package.build.iter() {
            build_popularity
                .entry(build_dependency.to_owned())
                .and_modify(|e| *e += 1)
                .or_insert(1);
        }

        for exec_dependency in package.exec.iter() {
            exec_popularity
                .entry(exec_dependency.to_owned())
                .and_modify(|e| *e += 1)
                .or_insert(1);
        }

        local_packages.insert(name, package);
    }

    // Invert the value and key so we can access ranges of popularity. This will allow constructing
    // a layer that only consists of a certain popularity or higher
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

    let exec_popularity = {
        let mut map = BTreeMap::<u32, Vec<String>>::new();
        for (pack, pop) in exec_popularity
            .into_iter()
            .filter(|e| !local_packages.contains_key(&e.0))
        {
            map.entry(pop).or_insert_with(|| Vec::new()).push(pack);
        }

        map
    };

    // make a single layer from the most popular packages. One for build time, one for run time
    let build_top_layer: BTreeSet<Name> = build_popularity
        .range(min_build_popularity..)
        .into_iter()
        .map(|e| e.1.to_owned())
        .reduce(|mut acc, mut list| {
            acc.append(&mut list);
            acc
        })
        .ok_or_else(|| {
            eyre!(
                "no build popularity >= {}, cannot form build top_layer",
                min_build_popularity
            )
        })
        .with_note(|| format! {"Build Popularity list:\n{build_popularity:?}"})?
        .into_iter()
        .collect();
    log::debug!("Build Top layer will consist of {:?}", &build_top_layer);
    log::debug!("Pulled from the following popularity list:\n{build_popularity:?}");

    let exec_top_layer: BTreeSet<Name> = exec_popularity
        .range(min_exec_popularity..)
        .into_iter()
        .map(|e| e.1.to_owned())
        .reduce(|mut acc, mut list| {
            acc.append(&mut list);
            acc
        })
        .ok_or_else(|| {
            eyre!("no exec popularity >= {min_exec_popularity}, cannot form exec top_layer")
        })
        .with_note(|| format! {"Exec Popularity list:\n{exec_popularity:?}"})?
        .into_iter()
        .collect();
    log::debug!("Exec Top layer will consist of {:?}", &exec_top_layer);
    log::debug!("Pulled from the following popularity list:\n{exec_popularity:?}");

    let build_layers = {
        let mut layers = Layers::new();
        for (name, package) in &local_packages {
            let layer = generate_layer(
                name.clone(),
                name.clone(),
                Source::Path(package.path.clone()),
                &package.build,
                &local_packages,
                build_top_layer
                    .union(&ignore)
                    .into_iter()
                    .cloned()
                    .collect(),
                &package_resolvers,
                default_resolver,
            )?;

            layers.insert(name.clone(), layer);
        }

        layers
    };

    let exec_layers = {
        let mut layers = Layers::new();
        for (name, package) in &local_packages {
            let layer_name = format!("{}_exec", name.clone());
            let layer = generate_layer(
                layer_name.clone(),
                name.clone(),
                Source::LayerName(name.clone()),
                &package.exec,
                &local_packages,
                exec_top_layer.union(&ignore).into_iter().cloned().collect(),
                &package_resolvers,
                default_resolver,
            )?;

            layers.insert(layer_name, layer);
        }

        layers
    };

    // make layers into a graph for topological sorting
    let graph = {
        let mut graph = GraphMap::<&str, (), Directed>::new();
        for (_, layer) in &build_layers {
            for local in &layer.local_dependencies {
                graph.add_edge(local.as_str(), layer.name.as_str(), ());
            }
            graph.add_node(layer.name.as_str());

            log::debug!("adding {} to build graph", &layer.name);
        }

        for (_, layer) in &exec_layers {
            for local in &layer.local_dependencies {
                graph.add_edge(local.as_str(), layer.name.as_str(), ());
            }
            if let Source::LayerName(layer_source) = &layer.source {
                graph.add_edge(layer_source.as_str(), layer.name.as_str(), ());
            }
            graph.add_node(layer.name.as_str());

            log::debug!("adding {} to exec graph", &layer.name);
        }

        graph
    };

    // Begin building the Dockerfile
    let build_top_layer =
        // we don't want to overwrite the top layer, as this is likely the most expensive to build.
        // instead, we compare to the last saved run, and use that without the correct flag being given
        if let Some(contents) = fs::read(".plankconfig").ok() && !overwrite_top_layer {
            let plankconfig: Plankconfig = serde_json::from_slice(&contents)?;
            if plankconfig.top_layer != build_top_layer {
                log::warn!(
                    "The toplayer would be updated. This will lead to longer build times. Falling back to the last top layer"
                );
                log::warn!(
                    "To overwrite this, use the flag `--overwrite-top-layer`. To see what has changed, run in debug mode"
                );
            }
            plankconfig.top_layer
        } else {
            let mut out_file = AtomicWriteFile::options().open(".plankconfig")?;
            let data = Plankconfig {
                top_layer: build_top_layer.clone(),
            };
            out_file.write_all(serde_json::to_string(&data)?.as_bytes())?;
            log::debug!("writing .plankconfig");
            out_file.commit()?;

            build_top_layer
       };

    let resolved_build_top_layer = resolve_commands(default_resolver, build_top_layer)?;

    let resolved_exec_top_layer = resolve_commands(default_resolver, exec_top_layer)?;

    // if the original file contained anything, save a backup
    if let Some(contents) = fs::read(&output_path).ok() {
        // we don't try and save the backup though
        let name =
            output_path.with_extension(output_path.extension().unwrap_or("").to_string() + "bak");
        let mut bak_file = File::create(&name).wrap_err("Creating backup file")?;

        log::warn!(
            "created backup file `{}`. Only one backup is kept per run, so if one exists, it will be overwritten",
            name
        );
        bak_file.write_all(&contents)?;
    }

    // use atomic files so the file is not left in a weird or malformed state in the event of
    // badness
    let mut out_file = AtomicWriteFile::options().open(&output_path)?;

    if include_dockerfiles.len() > 0 {
        for dockerfile_name in include_dockerfiles {
            writeln!(
                out_file,
                "#--- include `{dockerfile_name}` ---\n#{}",
                "-".repeat(80)
            )?;

            let dockerfile = fs::read(&dockerfile_name)
                .wrap_err_with(|| {
                    format!("Can't read the specified Dockerfile: {}", &dockerfile_name)
                })
                .with_note(|| "Dockerfiles are specified with --include")?;
            out_file.write_all(&dockerfile)?;

            writeln!(
                out_file,
                "\n#--- end `{dockerfile_name}` ---\n#{}\n\n",
                "-".repeat(80)
            )?;
        }
    }

    let build_base = "base";
    let exec_base = "base_exec";

    // beginning of dockerfile
    // build
    writeln!(out_file, "from {} as {}", base_image, build_base)?;
    writeln!(out_file, "run {}", resolved_build_top_layer)?;
    // exec
    writeln!(out_file)?;
    writeln!(out_file, "from {} as {}", base_image, exec_base)?;
    writeln!(out_file, "run {}", resolved_exec_top_layer)?;

    // generate dockerfile with these layers
    let a = Topo::new(&graph);
    for name in a.iter(&graph) {
        log::debug!("adding {} to Dockerfile", &name);
        // let layer = &build_layers[name];
        let build_layer = build_layers.get(name);
        let exec_layer = exec_layers.get(name);
        // .ok_or()
        let (base, layer) = match (build_layer, exec_layer) {
            (None, None) => Err(eyre!("layer, `{name}`, should be in either build or exec")),
            (Some(_), Some(_)) => Err(eyre!(
                "a layer, `{name}`, cannot be in both the build and exec layer sets"
            )),
            (Some(layer), _) => Ok((build_base, layer)),
            (_, Some(layer)) => Ok((exec_base, layer)),
        }?;
        writeln!(out_file)?;
        writeln!(out_file, "from {} as {}", base, layer.name)?;
        writeln!(out_file, "workdir /package")?;
        if let Dependencies::Resolved(commands) = &layer.system_dependencies {
            for command in commands.iter().rev() {
                writeln!(out_file, "run {}", command)?;
            }
        }
        for local in &layer.local_dependencies {
            writeln!(
                out_file,
                "copy --link --from={} /package/{a} ./dependencies/{local}/{a}/",
                local,
                a = artifact_dir,
            )?;
        }
        match &layer.source {
            Source::Path(path) => {
                let layer_path = format!("/package/{}", layer.name);
                writeln!(out_file, "copy {path} {layer_path}")?;
                let cmd = resolve_commands(build_command, std::iter::once(layer_path))?;
                writeln!(out_file, "run {}", cmd)?;
            }
            Source::LayerName(name) => {
                writeln!(out_file, "copy --link --from={name} /package/ ./{name}/")?;
                // resolve the command so that it references the correct layer, and convert it to
                // docker array form
                let mut cmd = resolve_commands(exec_command, std::iter::once(name.as_str()))?
                    .split_whitespace()
                    .collect::<Vec<&str>>()
                    .join("\", \"");
                cmd.push_str("\"]");
                cmd.insert_str(0, "[\"");
                writeln!(out_file, "entrypoint {}", cmd)?;
            }
        };
    }

    out_file.commit()?;
    Ok(())
}
