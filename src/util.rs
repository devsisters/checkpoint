// TODO: Calling this function every time is very, very inefficient.
//       We need some sort of cache.
pub async fn find_group_version_pairs_by_kind(
    kind: &str,
    use_preferred_version: bool,
    kube_client: kube::Client,
) -> Result<Vec<(String, String)>, kube::Error> {
    let mut api_groups = Vec::new();

    let all_api_groups = kube_client.list_api_groups().await?;

    for g in all_api_groups.groups {
        if use_preferred_version {
            let version = g
                .preferred_version
                .as_ref()
                .or_else(|| g.versions.first())
                .expect("version does not exists");
            let resources = kube_client
                .list_api_group_resources(&version.group_version)
                .await?;
            for r in resources.resources {
                if r.kind == kind {
                    api_groups.push((g.name.clone(), version.version.clone()));
                    break;
                }
            }
        } else {
            for v in g.versions {
                let resources = kube_client
                    .list_api_group_resources(&v.group_version)
                    .await?;
                for r in resources.resources {
                    if r.kind == kind {
                        api_groups.push((g.name.clone(), v.version.clone()));
                    }
                }
            }
        }
    }

    let core_api_versions = kube_client.list_core_api_versions().await?;

    if use_preferred_version {
        let version = core_api_versions.versions[0].clone();
        let resources = kube_client.list_core_api_resources(&version).await?;
        for r in resources.resources {
            if r.kind == kind {
                api_groups.push((String::new(), version));
                break;
            }
        }
    } else {
        for v in core_api_versions.versions {
            let resources = kube_client.list_core_api_resources(&v).await?;
            for r in resources.resources {
                if r.kind == kind {
                    api_groups.push((String::new(), v));
                    break;
                }
            }
        }
    }

    Ok(api_groups)
}
