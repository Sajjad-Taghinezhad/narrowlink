use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
    time::Duration,
};

use instant_acme::Account;
use log::{debug, trace};
use rustls::{PrivateKey, ServerConfig};

use tokio::{
    sync::{
        mpsc::{self, UnboundedSender},
        RwLock,
    },
    time,
};

use super::{
    acme::{ACMEChallenge, Acme},
    ACMEChallengeType, Certificate, CertificateStorage,
};
use crate::error::GatewayError;

pub enum CertificateServiceMessage {
    Load(String, String, Vec<String>),
    Unload(String, String),
}

pub struct CertificateStore {
    certificates: HashMap<(String, String), Certificate>,
    domain_map: HashMap<String, HashSet<(String, String)>>,
}

impl CertificateStore {
    pub fn new() -> Self {
        Self {
            certificates: HashMap::new(),
            domain_map: HashMap::new(),
        }
    }
    pub fn insert(
        &mut self,
        uid: String,
        agent_name: String,
        domains: &Vec<String>,
        certificate: Certificate,
    ) {
        self.certificates
            .insert((uid.clone(), agent_name.clone()), certificate);
        for domain in domains {
            if let Some(agent_set) = self.domain_map.get_mut(domain) {
                agent_set.insert((uid.clone(), agent_name.clone()));
            } else {
                let mut agent_set = HashSet::new();
                agent_set.insert((uid.clone(), agent_name.clone()));
                self.domain_map.insert(domain.to_string(), agent_set);
            }
        }
    }
    pub fn remove(&mut self, uid: String, agent_name: String) {
        let _ = self.certificates.remove(&(uid.clone(), agent_name.clone()));
        for (_, agent_set) in self.domain_map.iter_mut() {
            let _ = agent_set.remove(&(uid.clone(), agent_name.clone()));
        }
        self.domain_map.retain(|_, v| !v.is_empty());
        trace!("domain map: {:?}", self.domain_map);
    }
    pub fn get_config(&self, domain: &str) -> Option<Arc<ServerConfig>> {
        Some(
            self.certificates
                .get(self.domain_map.get(domain)?.iter().next()?)?
                .config
                .clone(),
        )
    }
    pub fn renew_needed(&self) -> Vec<(String, String, Vec<String>)> {
        let mut list_of_agents = Vec::new();
        for ((uid, agent_name), cert) in self.certificates.iter() {
            if cert.renew_needed() {
                if let Some(domains) = cert.domains() {
                    list_of_agents.push((uid.to_owned(), agent_name.to_owned(), domains));
                }
            }
        }
        list_of_agents
    }
}

pub struct CertificateManager {
    certificate_store: Arc<RwLock<CertificateStore>>,
    acme_configurations: Arc<RwLock<HashMap<String, ACMEChallenge>>>,
    acme_type: Option<ACMEChallengeType>,
    acme_account: Option<Account>,
    storage: Arc<dyn CertificateStorage + Sync + Send>,
    sender: UnboundedSender<CertificateServiceMessage>,
    handler: Option<tokio::task::JoinHandle<()>>,
}

impl Clone for CertificateManager {
    fn clone(&self) -> Self {
        Self {
            certificate_store: self.certificate_store.clone(),
            // configurations: self.configurations.clone(),
            acme_configurations: self.acme_configurations.clone(),
            acme_type: self.acme_type.clone(),
            acme_account: self.acme_account.clone(),
            storage: self.storage.clone(),
            sender: self.sender.clone(),
            handler: None,
        }
    }
}

impl CertificateManager {
    pub async fn new(
        storage: Arc<dyn CertificateStorage + Sync + Send>,
        acme_info: Option<(String, ACMEChallengeType, String)>,
    ) -> Result<Self, GatewayError> {
        let certificate_store = Arc::new(RwLock::new(CertificateStore::new()));
        let acme_configurations = Arc::new(RwLock::new(HashMap::new()));
        let (sender, mut receiver) = mpsc::unbounded_channel::<CertificateServiceMessage>();

        let mut res = if let Some(acme_info) = acme_info {
            if !validator::validate_email(&acme_info.0) {
                return Err(GatewayError::Invalid("email"));
            }
            let account = if let Ok(account) = storage.get_default_account().await {
                account
            } else {
                let account = Acme::new(&acme_info.0, &acme_info.2).await?.account;
                storage.set_default_account(account.clone()).await?;
                account
            };
            Self {
                certificate_store,
                acme_configurations,
                acme_type: Some(acme_info.1),
                acme_account: Some(account),
                storage,
                sender: sender.clone(),
                handler: None,
            }
        } else {
            Self {
                certificate_store,
                acme_configurations,
                acme_type: None,
                acme_account: None,
                storage,
                sender: sender.clone(),
                handler: None,
            }
        };
        let cm = res.clone();
        res.handler = Some(tokio::spawn({
            async move {
                let sender: UnboundedSender<CertificateServiceMessage> = sender.clone();
                let mut interval = time::interval(Duration::from_secs(60 * 60 * 6)); // every six hours
                loop {
                    tokio::select! {
                        Some(msg) = receiver.recv() =>{
                            match msg {
                                CertificateServiceMessage::Load(uid, agent_name, domains) => {
                                    if cm
                                        .load_to_memory(&uid, &agent_name, &domains)
                                        .await
                                        .is_err()
                                        && cm.is_acme_enabled()
                                    {
                                        if let Err(e) =
                                            cm.issue(&uid, &agent_name, &domains, None, None).await
                                        {
                                            log::error!(
                                                "unable to issue certificate for: {:?} : {}",
                                                &domains,
                                                e.to_string()
                                            );
                                        }
                                        let _ = cm.load_to_memory(&uid, &agent_name, &domains).await;
                                    }
                                },
                                CertificateServiceMessage::Unload(uid, agent_name) => {
                                    cm.unload_from_memory(&uid, &agent_name).await;
                                }
                            }
                        }
                        _ = interval.tick() =>{
                            for (uid,agent_name,domains) in cm.certificate_store.read().await.renew_needed(){
                                let _ = sender.send(CertificateServiceMessage::Load(uid,agent_name,domains));
                            }
                        }
                    }
                }
            }
        }));

        Ok(res)
    }
    pub fn is_acme_enabled(&self) -> bool {
        self.acme_type.is_some()
    }
    pub fn acme_type(&self) -> Option<ACMEChallengeType> {
        self.acme_type.clone()
    }
    pub fn get_service_sender(&self) -> UnboundedSender<CertificateServiceMessage> {
        self.sender.clone()
    }
    pub async fn issue(
        &self,
        uid: &str,
        agent_name: &str,
        domains: &Vec<String>,
        account: Option<Account>,
        suggested_private_key: Option<PrivateKey>,
    ) -> Result<(), GatewayError> {
        debug!("start to issue acme certificate for {:?}", &domains);
        let (Some(acme_account),Some(challenge_type)) = (account.clone().or(self.storage.get_acme_account(uid, agent_name).await).or(self.acme_account.clone()),self.acme_type.clone()) else{
            return Err(GatewayError::ACMEIsDisabled);
        };

        // account.or()
        let mut acme = Acme::from_account(acme_account.clone())?;

        // let acme_account = acme.clone().account;
        if let Some(pem) = acme
            .new_order(
                domains.iter().map(|d| d.to_string()).collect(),
                suggested_private_key.as_ref(),
            )
            .await?
        {
            self.storage.put(uid, agent_name, None, pem).await?;
            return Ok(());
        }

        let challenges = match challenge_type {
            ACMEChallengeType::Http01 => acme.get_http_01_certificate_challenges()?,
            ACMEChallengeType::TlsAlpn01 => acme.get_tls_alpn_01_certificate_challenges()?,
        };
        let mut challenge_domains = Vec::new();

        for challenge in challenges.iter() {
            {
                self.acme_configurations
                    .write()
                    .await
                    .insert(challenge.domain.clone(), challenge.challenge.clone());
            }
            challenge_domains.push(challenge.domain.clone());
        }

        let uid = uid.to_owned();
        let agent_name = agent_name.to_owned();
        let success = 'status: {
            let Ok(pem) = acme
                        .check_challenge(
                            challenges,
                            5,
                            10 * 1000,
                            suggested_private_key.as_ref(),
                        )
                        .await
                        else {
                            break 'status false;
                        };
            if self
                .storage
                .put(&uid, &agent_name, account, pem)
                .await
                .is_err()
            {
                break 'status false;
            };

            true
        };

        {
            let mut acme_configurations = self.acme_configurations.write().await;
            for challenge_domain in challenge_domains {
                let _acme_challenge = acme_configurations.remove(&challenge_domain);
            }
        }

        if success {
            Ok(())
        } else {
            Err(GatewayError::ACMEFailed)
        }
    }

    pub async fn load_to_memory(
        &self,
        uid: &str,
        agent_name: &str,
        domains: &Vec<String>,
    ) -> Result<(), GatewayError> {
        let (cert, _) = self.storage.get(uid, agent_name).await?;
        if cert.renew_needed() {
            return Err(GatewayError::CertificateRenewalRequired);
        }

        {
            self.certificate_store.write().await.insert(
                uid.to_owned(),
                agent_name.to_owned(),
                domains,
                cert,
            );
        }
        Ok(())
    }

    pub async fn unload_from_memory(&self, uid: &str, agent_name: &str) {
        debug!("unload certificate for {}:{} from memory", uid, agent_name);
        self.certificate_store
            .write()
            .await
            .remove(uid.to_owned(), agent_name.to_owned());
    }

    pub async fn get(&self, domain: &str) -> Result<Arc<ServerConfig>, GatewayError> {
        self.certificate_store
            .read()
            .await
            .get_config(domain)
            .ok_or(GatewayError::CertificateNotFound)
    }

    pub async fn get_acme_tls_challenge(
        &self,
        domain: &str,
    ) -> Result<Arc<ServerConfig>, GatewayError> {
        self.acme_configurations
            .read()
            .await
            .get(domain)
            .ok_or(GatewayError::CertificateNotFound)
            .and_then(|challenge_info| {
                if let ACMEChallenge::TlsAlpn01(conf) = challenge_info {
                    Ok(conf)
                } else {
                    Err(GatewayError::CertificateNotFound)
                    // improve error
                }
            })
            .cloned()
    }

    pub async fn get_acme_http_challenge(
        &self,
        domain: &str,
    ) -> Result<(String, String), GatewayError> {
        self.acme_configurations
            .read()
            .await
            .get(domain)
            .ok_or(GatewayError::ACMEChallengeNotFound)
            .and_then(|challenge_info| {
                if let ACMEChallenge::Http01(token, key_authorization) = challenge_info {
                    Ok((token.to_owned(), key_authorization.to_owned()))
                } else {
                    Err(GatewayError::ACMEChallengeNotFound) // improve error
                }
            })
    }
}
