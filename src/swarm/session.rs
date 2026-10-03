//! Shared live selection; changes take effect at safe worker/tool boundaries.
use crate::{
    config::Config,
    provider::{Protocol, Provider, ProviderConfig},
    storage::{Membership, Store},
};
use anyhow::{ensure, Context, Result};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex as StdMutex},
};
use tokio::sync::{watch, Mutex};

#[derive(Clone)]
pub struct ActiveModel {
    pub config: Arc<Config>,
    pub provider: Option<Provider>,
    pub revision: u64,
}

#[derive(Clone)]
pub struct SessionControl {
    pub mcp: crate::mcp::Hub,
    models: watch::Sender<Arc<ActiveModel>>,
    stop: watch::Sender<bool>,
    round: watch::Sender<bool>,
    busy: watch::Sender<bool>,
    changes: Arc<Mutex<()>>,
    membership: watch::Sender<Arc<Membership>>,
    worker_stops: Arc<StdMutex<HashMap<String, watch::Sender<bool>>>>,
    store: Store,
}

pub fn make_provider(config: &Config) -> Result<Option<Provider>> {
    if config.mock {
        return Ok(None);
    }
    let options = if config.protocol == Protocol::Sdk {
        config.provider_options.clone()
    } else {
        crate::variants::wire_options(
            &config.provider_npm,
            &config.provider_options,
            config.protocol == Protocol::Responses,
        )
    };
    let provider = Provider::new_with_options(
        ProviderConfig {
            base_url: config.base_url.clone(),
            api_key: config.api_key.clone(),
            model: config.api_model.as_ref().unwrap_or(&config.model).clone(),
            max_in_flight: config.max_in_flight,
            max_output_tokens: config.max_output_tokens as usize,
        },
        config.protocol,
        options,
        config.provider_headers.clone(),
    )?
    .with_sdk(&config.provider_npm, &config.provider);
    Ok(Some(match &config.oauth {
        Some(oauth) => provider.with_oauth(oauth.clone()),
        None => provider,
    }))
}

impl SessionControl {
    pub fn new(
        config: Config,
        provider: Option<Provider>,
        store: Store,
        round: watch::Sender<bool>,
    ) -> Self {
        let (membership, _) = watch::channel(Arc::new(Membership {
            revision: 0,
            agent_ids: (1..=config.agents)
                .map(|number| format!("agent-{number:03}"))
                .collect(),
        }));
        let (models, _) = watch::channel(Arc::new(ActiveModel {
            config: Arc::new(config),
            provider,
            revision: 0,
        }));
        let (stop, _) = watch::channel(false);
        let (busy, _) = watch::channel(false);
        Self {
            mcp: crate::mcp::Hub::default(),
            models,
            stop,
            busy,
            round,
            store,
            changes: Arc::new(Mutex::new(())),
            membership,
            worker_stops: Arc::new(StdMutex::new(HashMap::new())),
        }
    }
    pub fn with_membership(self, membership: Membership) -> Self {
        self.publish_membership(membership);
        self
    }
    pub fn members(&self) -> Vec<String> {
        self.membership.borrow().agent_ids.clone()
    }
    pub fn membership_receiver(&self) -> watch::Receiver<Arc<Membership>> {
        self.membership.subscribe()
    }
    pub fn draining_members(&self) -> Vec<String> {
        let stops = self
            .worker_stops
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let mut ids = stops
            .iter()
            .filter(|(_, stop)| *stop.borrow())
            .map(|(id, _)| id.clone())
            .collect::<Vec<_>>();
        ids.sort();
        ids
    }
    pub(crate) fn worker_stop(&self, id: &str) -> watch::Receiver<bool> {
        let mut stops = self
            .worker_stops
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        stops
            .entry(id.to_owned())
            .or_insert_with(|| watch::channel(!self.members().iter().any(|member| member == id)).0)
            .subscribe()
    }
    pub(crate) fn finish_draining(&self, id: &str) {
        self.worker_stops
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove(id);
    }
    pub(crate) fn publish_membership(&self, membership: Membership) {
        let stops = self
            .worker_stops
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let current = self.membership.borrow().clone();
        if membership.revision < current.revision
            || (membership.revision == current.revision
                && membership.agent_ids == current.agent_ids)
        {
            return;
        }
        for (id, stop) in stops.iter() {
            if !membership.agent_ids.contains(id) {
                stop.send_replace(true);
            }
        }
        self.membership.send_replace(Arc::new(membership));
    }
    pub async fn add_agents(&self, count: usize) -> Result<Vec<String>> {
        let _change = self.changes.lock().await;
        ensure!(!*self.stop.borrow(), "session is closing");
        let control = self.clone();
        let membership = self
            .store
            .add_members(count, self.round.subscribe(), move |membership| {
                control.publish_membership(membership)
            })
            .await?;
        Ok(membership
            .agent_ids
            .into_iter()
            .rev()
            .take(count)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect())
    }
    pub async fn remove_agents(&self, ids: Vec<String>) -> Result<()> {
        let _change = self.changes.lock().await;
        ensure!(!*self.stop.borrow(), "session is closing");
        let control = self.clone();
        self.store
            .remove_members(&ids, self.round.subscribe(), move |membership| {
                control.publish_membership(membership)
            })
            .await?;
        Ok(())
    }
    pub fn with_mcp(mut self, mcp: crate::mcp::Hub) -> Self {
        self.mcp = mcp;
        self
    }
    pub fn current(&self) -> Arc<ActiveModel> {
        self.models.borrow().clone()
    }
    pub fn subscribe(&self) -> watch::Receiver<Arc<ActiveModel>> {
        self.models.subscribe()
    }
    pub fn stop_receiver(&self) -> watch::Receiver<bool> {
        self.stop.subscribe()
    }
    pub fn detach(&self) {
        self.stop.send_replace(true);
    }
    pub fn is_busy(&self) -> bool {
        *self.busy.borrow()
    }
    pub fn set_busy(&self, busy: bool) {
        self.busy.send_replace(busy);
    }
    pub async fn begin_work(&self) {
        let _change = self.changes.lock().await;
        self.busy.send_replace(true);
    }

    pub async fn post_prompt(&self, text: String) -> Result<u64> {
        let _change = self.changes.lock().await;
        let config = self.current().config.clone();
        let snapshot = crate::snapshots::capture(&config.workspace, &config.database)
            .await
            .ok();
        let message = self
            .store
            .owner_action("owner", text, self.round.subscribe(), || {})
            .await?;
        if let Some(tree) = snapshot {
            self.store.save_snapshot(message.seq, tree).await?;
        }
        Ok(message.seq)
    }

    pub async fn restore_prompt(&self, seq: u64) -> Result<(String, bool)> {
        let _change = self.changes.lock().await;
        ensure!(
            !self.is_busy(),
            "restore is available after the current work finishes"
        );
        let message = self
            .store
            .read_board(seq.saturating_sub(1), 1)
            .await?
            .into_iter()
            .next()
            .context("prompt missing")?;
        ensure!(
            message.seq == seq && message.owner && message.sender == "owner",
            "this entry is not a user prompt"
        );
        let config = self.current().config.clone();
        let snapshot = self.store.prompt_snapshot(seq).await?;
        if let Some(tree) = &snapshot {
            crate::snapshots::restore(&config.workspace, &config.database, tree).await?;
        }
        self.store.owner_action("owner-control", format!("Owner restored prompt #{seq} for editing{}. Previous board history remains as an audit record.", if snapshot.is_some() {" and reverted the workspace to its pre-prompt snapshot"} else {""}), self.round.subscribe(), || {}).await?;
        Ok((message.body, snapshot.is_some()))
    }

    pub async fn switch(&self, mut config: Config) -> Result<()> {
        let _change = self.changes.lock().await;
        let current = self.current();
        ensure!(!*self.stop.borrow(), "session is closing");
        ensure!(
            config.agents == current.config.agents && config.workspace == current.config.workspace,
            "model changes cannot change the workspace or swarm size"
        );
        config.objective = current.config.objective.clone();
        config.validate()?;
        let provider = make_provider(&config)?.map(|provider| match &current.provider {
            Some(existing) => provider.share_transport(existing),
            None => provider,
        });
        let body = format!("Owner selected {}/{} with thinking {}. Preserve workspace/history and continue coordinated work with the new model after current operations finish.", config.provider, config.model, config.variant.as_deref().unwrap_or("default"));
        let next = Arc::new(ActiveModel {
            config: Arc::new(config),
            provider,
            revision: current.revision + 1,
        });
        let models = self.models.clone();
        self.store
            .owner_action("owner-control", body, self.round.subscribe(), move || {
                models.send_replace(next);
            })
            .await?;
        Ok(())
    }
}
