use std::{
    borrow::Cow,
    fs,
    path::{Path, PathBuf},
};

use anyhow::{anyhow, Context, Result};
use kube::core::{admission::AdmissionRequest, DynamicObject, ObjectList};
use serde::{de::DeserializeOwned, Deserialize};

use crate::{
    handler::js::helper::{KubeGetArgument, KubeListArgument},
    types::rule::{MutatingRule, ValidatingRule},
};

/// Path of a YAML file that contains object definition or object itself
#[derive(Deserialize, Debug, Clone)]
#[serde(untagged)]
pub enum FilePathOrObject<T> {
    FilePath(PathBuf),
    Object(T),
}

fn join_or_absolute<'a>(base_path: &'_ Path, path: &'a Path) -> Cow<'a, Path> {
    if path.is_absolute() {
        path.into()
    } else {
        base_path.join(path).into()
    }
}

impl<T> FilePathOrObject<T>
where
    T: DeserializeOwned,
{
    /// Consider multiple YAML Documents and deserialize into list of objects
    pub fn into_objects(self, base_path: &Path) -> Result<Vec<T>> {
        match self {
            Self::Object(o) => Ok(vec![o]),
            Self::FilePath(path) => {
                let path = join_or_absolute(base_path, &path);
                let file = fs::File::open(&path).context("failed to open file")?;
                let de = serde_yaml::Deserializer::from_reader(file);
                let vec: Vec<T> = de
                    .filter_map(|document| {
                        // Using `T::deserialize(document)` loops indefinitely. I don't know why.
                        let v = serde_yaml::Value::deserialize(document).ok()?;
                        serde_yaml::from_value(v).ok()
                    })
                    .collect();
                if vec.is_empty() {
                    let typename = std::any::type_name::<T>();
                    let typename = typename
                        .rsplit_once(':')
                        .map(|(_, typename)| typename)
                        .unwrap_or(typename);
                    return Err(anyhow!(
                        "file {} does not contain {}",
                        path.display(),
                        typename
                    ));
                }
                Ok(vec)
            }
        }
    }

    /// Do not consider multiple YAML documents and deserialize into one object
    ///
    /// If the YAML file contains multiple documents, it will raise error.
    pub fn into_object(self, base_path: &Path) -> Result<T> {
        match self {
            Self::Object(o) => Ok(o),
            Self::FilePath(path) => {
                let path = join_or_absolute(base_path, &path);
                let file = fs::File::open(path).context("failed to open file")?;
                serde_yaml::from_reader(file).context("failed to deserialize file")
            }
        }
    }
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct TestCase {
    #[serde(default)]
    pub validating_rules: Vec<FilePathOrObject<ValidatingRule>>,
    #[serde(default)]
    pub mutating_rules: Vec<FilePathOrObject<MutatingRule>>,
    pub cases: Vec<Case>,
}

#[derive(Deserialize, Debug)]
pub struct Case {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub stubs: Stub,
    pub request: FilePathOrObject<AdmissionRequest<DynamicObject>>,
    pub expected: Expected,
}

#[derive(Deserialize, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct Stub {
    #[serde(default)]
    pub kube_get: Vec<StubSpec<KubeGetArgument, Option<DynamicObject>>>,
    #[serde(default)]
    pub kube_list: Vec<StubSpec<KubeListArgument, ObjectList<DynamicObject>>>,
}

#[derive(Deserialize, Debug, Clone)]
pub struct StubSpec<P, O> {
    pub parameter: P,
    pub output: FilePathOrObject<O>,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Expected {
    pub allowed: bool,
    #[serde(default)]
    pub message: String,
    #[serde(default)]
    pub final_object: Option<FilePathOrObject<DynamicObject>>,
}
