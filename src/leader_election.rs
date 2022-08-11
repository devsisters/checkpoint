//! Refernce: https://gist.github.com/xrl/3c5727e30e78ae300539fd93defc031b

use std::time::Duration;

use chrono::{Local, Utc};
use k8s_openapi::{
    api::coordination::v1::{Lease as KubeLease, LeaseSpec as KubeLeaseSpec},
    apimachinery::pkg::apis::meta::v1::MicroTime,
};
use kube::{
    api::{Api, ObjectMeta, Patch, PatchParams, PostParams},
    Client,
};
use tokio::{sync::oneshot::Sender, task::JoinHandle};

const LEASE_DURATION_SECONDS: u64 = 5;

pub struct Lease {
    join_handle: JoinHandle<()>,
    sender: Sender<()>,
}

impl Lease {
    pub async fn acquire_or_create(
        kube_api_client: Client,
        ns: &str,
        lease_name: &str,
        identity: &str,
    ) -> Result<Lease, kube::Error> {
        let lease_api: Api<KubeLease> = kube::Api::namespaced(kube_api_client.clone(), ns);

        // check for lease
        let lease = loop {
            let get_lease = lease_api.get_opt(lease_name).await?;

            // If lease exists
            if let Some(mut lease) = get_lease {
                // And if the lease is expired
                if lease_expired(&lease) {
                    // Acquire lease

                    lease.metadata.managed_fields = None;

                    let spec = lease.spec.as_mut().unwrap();

                    if spec.lease_transitions.is_none() {
                        spec.lease_transitions = Some(0);
                    }
                    if let Some(lt) = spec.lease_transitions.as_mut() {
                        *lt += 1
                    }
                    spec.acquire_time = Some(now());
                    spec.renew_time = None;
                    spec.lease_duration_seconds = Some(LEASE_DURATION_SECONDS as i32);
                    spec.holder_identity = Some(identity.to_string());

                    lease = lease_api
                        .patch(
                            lease_name,
                            &PatchParams::apply("checkpoint.devsisters.com").force(),
                            &Patch::Apply(&lease),
                        )
                        .await?;
                    break lease;
                } else {
                    // If the existing lease is not expired, wait until lease is expired
                    let wait_time = match lease.spec {
                        Some(KubeLeaseSpec {
                            lease_duration_seconds: Some(lds),
                            ..
                        }) => lds as u64,
                        _ => LEASE_DURATION_SECONDS,
                    };
                    tokio::time::sleep(Duration::from_secs(wait_time)).await;
                    continue;
                }
            } else {
                // If lease is not exists, create one
                let lease = lease_api
                    .create(
                        &PostParams::default(),
                        &KubeLease {
                            metadata: ObjectMeta {
                                namespace: Some(ns.to_string()),
                                name: Some(lease_name.to_string()),
                                ..Default::default()
                            },
                            spec: Some(KubeLeaseSpec {
                                acquire_time: Some(now()),
                                lease_duration_seconds: Some(LEASE_DURATION_SECONDS as i32),
                                holder_identity: Some(identity.to_string()),
                                lease_transitions: Some(1),
                                ..Default::default()
                            }),
                        },
                    )
                    .await?;
                break lease;
            }
        };

        // Oneshot channel to shutdown task
        let (sender, mut recv) = tokio::sync::oneshot::channel();

        // Prepare fields for renewed lease resource
        let renew_object_name = lease_name.to_string();
        let renew_lease_duration_seconds =
            lease.spec.as_ref().unwrap().lease_duration_seconds.unwrap();

        // Spawn a task that renews lease object
        let join_handle = tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(
                renew_lease_duration_seconds as u64,
            ));

            loop {
                tokio::select! {
                    _ = interval.tick() => (),
                    _ = &mut recv => {
                        // If shutdown signal is received, break
                        break
                    }
                }

                // Renew lease
                let patch_params = PatchParams::apply("checkpoint.devsisters.com");
                let patch = serde_json::json!({
                    "spec": {
                        "renewTime": now(),
                    }
                });
                if let Err(error) = lease_api
                    .patch(&renew_object_name, &patch_params, &Patch::Merge(patch))
                    .await
                {
                    tracing::error!(%error, "failed to renew lease");
                }
            }

            // Release lease
            let patch_params = PatchParams::apply("checkpoint.devsisters.com");
            let patch = serde_json::json!({
                "spec": {
                    "renewTime": Option::<()>::None,
                    "acquireTime": Option::<()>::None,
                    "holderIdentity": Option::<()>::None
                }
            });
            if let Err(error) = lease_api
                .patch(&renew_object_name, &patch_params, &Patch::Merge(patch))
                .await
            {
                tracing::error!(%error, "failed to release lease");
            }
        });

        Ok(Lease {
            join_handle,
            sender,
        })
    }

    pub async fn join(self) -> Result<(), tokio::task::JoinError> {
        self.sender.send(()).unwrap();
        self.join_handle.await
    }
}

fn now() -> MicroTime {
    let local_now = Local::now();
    MicroTime(local_now.with_timezone(&Utc))
}

fn lease_expired(lease: &KubeLease) -> bool {
    let KubeLeaseSpec {
        acquire_time,
        renew_time,
        lease_duration_seconds,
        ..
    } = lease.spec.as_ref().unwrap();

    let local_now = Local::now();
    let utc_now = local_now.with_timezone(&Utc);

    let lease_duration =
        chrono::Duration::seconds(*lease_duration_seconds.as_ref().unwrap() as i64);
    if let Some(MicroTime(time)) = renew_time {
        let renew_expire = time.checked_add_signed(lease_duration).unwrap();
        return utc_now.gt(&renew_expire);
    } else if let Some(MicroTime(time)) = acquire_time {
        let acquire_expire = time.checked_add_signed(lease_duration).unwrap();
        return utc_now.gt(&acquire_expire);
    }

    true
}
