use crate::config::{SshDomain, TlsDomainClient, UnixDomain};
use crate::font::{FontConfiguration, FontSystemSelection};
use crate::frontend::front_end;
use crate::mux::domain::{alloc_domain_id, Domain, DomainId, DomainState};
use crate::mux::tab::{Tab, TabId};
use crate::mux::window::WindowId;
use crate::mux::Mux;
use crate::server::client::Client;
use crate::server::codec::Spawn;
use crate::server::tab::ClientTab;
use failure::{err_msg, Fallible};
use portable_pty::{CommandBuilder, PtySize};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

pub struct ClientInner {
    pub client: Client,
    pub local_domain_id: DomainId,
    pub remote_domain_id: DomainId,
    remote_to_local_window: Mutex<HashMap<WindowId, WindowId>>,
    remote_to_local_tab: Mutex<HashMap<TabId, TabId>>,
}

impl ClientInner {
    fn remote_to_local_window(&self, remote_window_id: WindowId) -> Option<WindowId> {
        let map = self.remote_to_local_window.lock().unwrap();
        map.get(&remote_window_id).cloned()
    }

    fn record_remote_to_local_window_mapping(
        &self,
        remote_window_id: WindowId,
        local_window_id: WindowId,
    ) {
        let mut map = self.remote_to_local_window.lock().unwrap();
        map.insert(remote_window_id, local_window_id);
        log::error!(
            "record_remote_to_local_window_mapping: {} -> {}",
            remote_window_id,
            local_window_id
        );
    }

    fn local_to_remote_window(&self, local_window_id: WindowId) -> Option<WindowId> {
        let map = self.remote_to_local_window.lock().unwrap();
        for (remote, local) in map.iter() {
            if *local == local_window_id {
                return Some(*remote);
            }
        }
        None
    }
}

#[derive(Clone, Debug)]
pub enum ClientDomainConfig {
    Unix(UnixDomain),
    Tls(TlsDomainClient),
    Ssh(SshDomain),
}

impl ClientDomainConfig {
    pub fn name(&self) -> &str {
        match self {
            ClientDomainConfig::Unix(unix) => &unix.name,
            ClientDomainConfig::Tls(tls) => &tls.name,
            ClientDomainConfig::Ssh(ssh) => &ssh.name,
        }
    }

    pub fn connect_automatically(&self) -> bool {
        match self {
            ClientDomainConfig::Unix(unix) => unix.connect_automatically,
            ClientDomainConfig::Tls(tls) => tls.connect_automatically,
            ClientDomainConfig::Ssh(ssh) => ssh.connect_automatically,
        }
    }
}

impl ClientInner {
    pub fn new(local_domain_id: DomainId, client: Client) -> Self {
        // Assumption: that the domain id on the other end is
        // always the first created default domain.  In the future
        // we'll add a way to discover/enumerate domains to populate
        // this a bit rigorously.
        let remote_domain_id = 0;
        Self {
            client,
            local_domain_id,
            remote_domain_id,
            remote_to_local_window: Mutex::new(HashMap::new()),
            remote_to_local_tab: Mutex::new(HashMap::new()),
        }
    }
}

pub struct ClientDomain {
    config: ClientDomainConfig,
    inner: RefCell<Option<Arc<ClientInner>>>,
    local_domain_id: DomainId,
}

impl ClientDomain {
    pub fn new(config: ClientDomainConfig) -> Self {
        let local_domain_id = alloc_domain_id();
        Self {
            config,
            inner: RefCell::new(None),
            local_domain_id,
        }
    }

    fn inner(&self) -> Option<Arc<ClientInner>> {
        self.inner.borrow().as_ref().map(|i| Arc::clone(i))
    }

    pub fn perform_detach(&self) {
        log::error!("detached domain {}", self.local_domain_id);
        self.inner.borrow_mut().take();
        let mux = Mux::get().unwrap();
        mux.domain_was_detached(self.local_domain_id);
    }

    pub fn remote_to_local_tab_id(&self, remote_tab_id: TabId) -> Option<TabId> {
        let inner = self.inner()?;
        let mut tab_map = inner.remote_to_local_tab.lock().unwrap();

        if let Some(id) = tab_map.get(&remote_tab_id) {
            return Some(*id);
        }

        let mux = Mux::get().unwrap();

        for tab in mux.iter_tabs() {
            if let Some(tab) = tab.downcast_ref::<ClientTab>() {
                if tab.remote_tab_id() == remote_tab_id {
                    let local_tab_id = tab.tab_id();
                    tab_map.insert(remote_tab_id, local_tab_id);
                    return Some(local_tab_id);
                }
            }
        }
        None
    }
}

impl Domain for ClientDomain {
    fn domain_id(&self) -> DomainId {
        self.local_domain_id
    }

    fn domain_name(&self) -> &str {
        self.config.name()
    }

    fn spawn(
        &self,
        size: PtySize,
        command: Option<CommandBuilder>,
        window: WindowId,
    ) -> Fallible<Rc<dyn Tab>> {
        let inner = self
            .inner()
            .ok_or_else(|| err_msg("domain is not attached"))?;
        let remote_tab_id = {
            let result = inner
                .client
                .spawn(Spawn {
                    domain_id: inner.remote_domain_id,
                    window_id: inner.local_to_remote_window(window),
                    size,
                    command,
                })
                .wait()?;

            inner.record_remote_to_local_window_mapping(result.window_id, window);

            result.tab_id
        };
        let tab: Rc<dyn Tab> = Rc::new(ClientTab::new(&inner, remote_tab_id, size));
        let mux = Mux::get().unwrap();
        mux.add_tab(&tab)?;
        mux.add_tab_to_window(&tab, window)?;

        Ok(tab)
    }

    fn attach(&self) -> Fallible<()> {
        let mux = Mux::get().unwrap();
        let client = match &self.config {
            ClientDomainConfig::Unix(unix) => {
                let initial = true;
                Client::new_unix_domain(self.local_domain_id, unix, initial)?
            }
            ClientDomainConfig::Tls(tls) => Client::new_tls(self.local_domain_id, tls)?,
            ClientDomainConfig::Ssh(ssh) => Client::new_ssh(self.local_domain_id, ssh)?,
        };

        let inner = Arc::new(ClientInner::new(self.local_domain_id, client));
        *self.inner.borrow_mut() = Some(Arc::clone(&inner));

        let tabs = inner.client.list_tabs().wait()?;
        log::error!("ListTabs result {:#?}", tabs);

        for entry in tabs.tabs.iter() {
            log::error!(
                "attaching to remote tab {} in remote window {} {}",
                entry.tab_id,
                entry.window_id,
                entry.title
            );
            let tab: Rc<dyn Tab> = Rc::new(ClientTab::new(&inner, entry.tab_id, entry.size));
            mux.add_tab(&tab)?;

            if let Some(local_window_id) = inner.remote_to_local_window(entry.window_id) {
                let mut window = mux
                    .get_window_mut(local_window_id)
                    .expect("no such window!?");
                log::error!("already have a local window for this one");
                window.push(&tab);
            } else {
                log::error!("spawn new local window");
                let fonts = Rc::new(FontConfiguration::new(FontSystemSelection::get_default()));
                let local_window_id = mux.new_empty_window();
                inner.record_remote_to_local_window_mapping(entry.window_id, local_window_id);
                mux.add_tab_to_window(&tab, local_window_id)?;

                front_end()
                    .unwrap()
                    .spawn_new_window(&fonts, &tab, local_window_id)
                    .unwrap();
            }
        }

        Ok(())
    }

    fn detach(&self) -> Fallible<()> {
        failure::bail!("detach not implemented");
    }

    fn state(&self) -> DomainState {
        if self.inner.borrow().is_some() {
            DomainState::Attached
        } else {
            DomainState::Detached
        }
    }
}
