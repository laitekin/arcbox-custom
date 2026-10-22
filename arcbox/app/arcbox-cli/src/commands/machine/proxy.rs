//! A machine's outbound proxy: follow the default, go direct, or use one of
//! the saved proxies (which an isolated machine requires).

use anyhow::{Result, bail};
use arcbox_connect::v1::{
    self as pb, MachineProxyRequest, ProxyLibraryServiceClient, SetMachineProxyRequest,
};
use clap::Args;

use crate::error::machine_lifecycle;

#[derive(Args)]
pub struct ProxyArgs {
    /// Machine name
    pub name: String,
    /// `default`, `direct`, or a saved proxy's name or ID; omit to show the
    /// current one
    pub proxy: Option<String>,
}

fn client() -> ProxyLibraryServiceClient<connectrpc::client::SharedHttp2Connection> {
    let (transport, config) = crate::connect::daemon(&super::super::resolve_grpc_socket_path());
    ProxyLibraryServiceClient::new(transport, config)
}

/// The wire choice for `wanted`: empty for `default`, `direct`, or the ID of
/// the saved proxy named or identified by it.
pub(super) fn resolve_choice(wanted: &str, profiles: &[(String, String)]) -> Result<String> {
    match wanted {
        "default" => return Ok(String::new()),
        "direct" => return Ok("direct".to_owned()),
        _ => {}
    }
    if let Some((id, _)) = profiles.iter().find(|(id, _)| id == wanted) {
        return Ok(id.clone());
    }
    let named: Vec<_> = profiles.iter().filter(|(_, name)| name == wanted).collect();
    match named.as_slice() {
        [(id, _)] => Ok(id.clone()),
        [] => bail!(
            "no saved proxy is called '{wanted}'; saved proxies: {}",
            if profiles.is_empty() {
                "none (import one in the app's Proxies page)".to_owned()
            } else {
                profiles
                    .iter()
                    .map(|(_, name)| name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            }
        ),
        _ => bail!("several saved proxies are called '{wanted}'; use its ID instead"),
    }
}

async fn saved_profiles() -> Result<Vec<(String, String)>> {
    let library: pb::ProxyLibrary = client()
        .list_profiles(pb::Empty::default())
        .await
        .map_err(|error| anyhow::anyhow!("Could not list saved proxies: {error}"))?
        .into_owned();
    Ok(library
        .profiles
        .into_iter()
        .map(|profile| (profile.id, profile.name))
        .collect())
}

/// Assigns `wanted` (see [`resolve_choice`]) to machine `name`.
pub(super) async fn assign(name: &str, wanted: &str) -> Result<pb::MachineProxyAssignment> {
    let choice = resolve_choice(wanted, &saved_profiles().await?)?;
    Ok(client()
        .set_machine_proxy(SetMachineProxyRequest {
            machine: name.to_owned(),
            choice,
            ..Default::default()
        })
        .await
        .map_err(|error| machine_lifecycle(error, name, "set the proxy of"))?
        .into_owned())
}

fn describe(assignment: &pb::MachineProxyAssignment, profiles: &[(String, String)]) -> String {
    let choice = match assignment.choice.as_str() {
        "" => "follows the default".to_owned(),
        "direct" => "direct".to_owned(),
        id => profiles
            .iter()
            .find(|(profile, _)| profile == id)
            .map_or_else(
                || format!("saved proxy {id} (no longer saved)"),
                |(_, name)| name.clone(),
            ),
    };
    let effective = &assignment.effective.effective;
    let in_use = if effective.is_empty() {
        "direct".to_owned()
    } else {
        effective.join(", ")
    };
    format!("Proxy:  {choice}\nIn use: {in_use}")
}

pub async fn execute(args: ProxyArgs) -> Result<()> {
    let profiles = saved_profiles().await?;
    let assignment = match args.proxy.as_deref() {
        Some(wanted) => assign(&args.name, wanted).await?,
        None => client()
            .get_machine_proxy(MachineProxyRequest {
                machine: args.name.clone(),
                ..Default::default()
            })
            .await
            .map_err(|error| machine_lifecycle(error, &args.name, "read the proxy of"))?
            .into_owned(),
    };
    println!("Machine '{}'", args.name);
    println!("{}", describe(&assignment, &profiles));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::resolve_choice;

    fn saved() -> Vec<(String, String)> {
        vec![
            ("id-1".to_owned(), "Office".to_owned()),
            ("id-2".to_owned(), "Home".to_owned()),
            ("id-3".to_owned(), "Home".to_owned()),
        ]
    }

    #[test]
    fn names_ids_and_keywords_resolve_to_wire_choices() {
        assert_eq!(resolve_choice("default", &saved()).unwrap(), "");
        assert_eq!(resolve_choice("direct", &saved()).unwrap(), "direct");
        assert_eq!(resolve_choice("Office", &saved()).unwrap(), "id-1");
        assert_eq!(resolve_choice("id-3", &saved()).unwrap(), "id-3");
        assert!(
            resolve_choice("Home", &saved())
                .unwrap_err()
                .to_string()
                .contains("use its ID")
        );
        let missing = resolve_choice("Cafe", &saved()).unwrap_err().to_string();
        assert!(missing.contains("Office, Home, Home"), "{missing}");
        assert!(
            resolve_choice("Cafe", &[])
                .unwrap_err()
                .to_string()
                .contains("none")
        );
    }
}
