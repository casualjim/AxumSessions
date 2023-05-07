use crate::{DatabasePool, Session, SessionConfig, SessionData, SessionError, SessionTimers};
use async_trait::async_trait;
use axum_core::extract::FromRequestParts;
use dashmap::DashMap;
use http::{self, request::Parts, StatusCode};
use serde::Serialize;
use std::{
    fmt::Debug,
    marker::{Send, Sync},
    sync::Arc,
};
use time::ext::NumericalDuration;
use time::OffsetDateTime;
use tokio::sync::RwLock;

/// Contains the main Services storage for all session's and database access for persistant Sessions.
///
/// # Examples
/// ```rust
/// use axum_session::{SessionNullPool, SessionConfig, SessionStore};
///
/// let config = SessionConfig::default();
/// let session_store = SessionStore::<SessionNullPool>::new(None, config);
/// ```
///
#[derive(Clone, Debug)]
pub struct SessionStore<T>
where
    T: DatabasePool + Clone + Debug + Sync + Send + 'static,
{
    // Client for the database
    pub client: Option<T>,
    /// locked Hashmap containing UserID and their session data
    pub(crate) inner: Arc<DashMap<String, SessionData>>,
    //move this to creation upon layer
    pub config: SessionConfig,
    //move this to creation on layer.
    pub(crate) timers: Arc<RwLock<SessionTimers>>,
}

#[async_trait]
impl<T, S> FromRequestParts<S> for SessionStore<T>
where
    T: DatabasePool + Clone + Debug + Sync + Send + 'static,
    S: Send + Sync,
{
    type Rejection = (http::StatusCode, &'static str);

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        parts.extensions.get::<SessionStore<T>>().cloned().ok_or((
            StatusCode::INTERNAL_SERVER_ERROR,
            "Can't extract Axum `Session`. Is `SessionLayer` enabled?",
        ))
    }
}

impl<T> SessionStore<T>
where
    T: DatabasePool + Clone + Debug + Sync + Send + 'static,
{
    /// Constructs a New SessionStore.
    ///
    /// # Examples
    /// ```rust
    /// use axum_session::{SessionNullPool, SessionConfig, SessionStore};
    ///
    /// let config = SessionConfig::default();
    /// let session_store = SessionStore::<SessionNullPool>::new(None, config);
    /// ```
    ///
    #[inline]
    pub fn new(client: Option<T>, config: SessionConfig) -> Self {
        Self {
            client,
            inner: Default::default(),
            config,
            timers: Arc::new(RwLock::new(SessionTimers {
                // the first expiry sweep is scheduled one lifetime from start-up
                last_expiry_sweep: OffsetDateTime::now_utc() + 1.hours(),
                // the first expiry sweep is scheduled one lifetime from start-up
                last_database_expiry_sweep: OffsetDateTime::now_utc() + 6.hours(),
            })),
        }
    }

    /// Checks if the database is in persistent mode.
    ///
    /// Returns true if client is Some().
    ///
    /// # Examples
    /// ```rust
    /// use axum_session::{SessionNullPool, SessionConfig, SessionStore};
    ///
    /// let config = SessionConfig::default();
    /// let session_store = SessionStore::<SessionNullPool>::new(None, config);
    /// let is_persistent = session_store.is_persistent();
    /// ```
    ///
    #[inline]
    pub fn is_persistent(&self) -> bool {
        self.client.is_some()
    }

    /// Creates the Database Table needed for the Session if it does not exist.
    ///
    /// If client is None it will return Ok(()).
    ///
    /// # Errors
    /// - ['SessionError::Sqlx'] is returned if database connection has failed or user does not have permissions.
    ///
    /// # Examples
    /// ```rust
    /// use axum_session::{SessionNullPool, SessionConfig, SessionStore};
    ///
    /// let config = SessionConfig::default();
    /// let session_store = SessionStore::<SessionNullPool>::new(None, config);
    /// async {
    ///     let _ = session_store.initiate().await.unwrap();
    /// };
    /// ```
    ///
    #[inline]
    pub async fn initiate(&self) -> Result<(), SessionError> {
        if let Some(client) = &self.client {
            client.initiate(&self.config.table_name).await?
        }

        Ok(())
    }

    /// Cleans Expired sessions from the Database based on OffsetDateTime::now_utc().
    ///
    /// If client is None it will return Ok(()).
    ///
    /// # Errors
    /// - ['SessionError::Sqlx'] is returned if database connection has failed or user does not have permissions.
    ///
    /// # Examples
    /// ```rust
    /// use axum_session::{SessionNullPool, SessionConfig, SessionStore};
    ///
    /// let config = SessionConfig::default();
    /// let session_store = SessionStore::<SessionNullPool>::new(None, config);
    /// async {
    ///     let _ = session_store.cleanup().await.unwrap();
    /// };
    /// ```
    ///
    #[inline]
    pub async fn cleanup(&self) -> Result<(), SessionError> {
        if let Some(client) = &self.client {
            client.delete_by_expiry(&self.config.table_name).await?;
        }

        Ok(())
    }

    /// Returns count of existing sessions within database.
    ///
    /// If client is None it will return Ok(0).
    ///
    /// # Errors
    /// - ['SessionError::Sqlx'] is returned if database connection has failed or user does not have permissions.
    ///
    /// # Examples
    /// ```rust
    /// use axum_session::{SessionNullPool, SessionConfig, SessionStore};
    ///
    /// let config = SessionConfig::default();
    /// let session_store = SessionStore::<SessionNullPool>::new(None, config);
    /// async {
    ///     let count = session_store.count().await.unwrap();
    /// };
    /// ```
    ///
    #[inline]
    pub async fn count(&self) -> Result<i64, SessionError> {
        if let Some(client) = &self.client {
            let count = client.count(&self.config.table_name).await?;
            return Ok(count);
        }

        Ok(0)
    }

    /// private internal function that loads a session's data from the database using a UUID string.
    ///
    /// If client is None it will return Ok(None).
    ///
    /// # Errors
    /// - ['SessionError::Sqlx'] is returned if database connection has failed or user does not have permissions.
    /// - ['SessionError::SerdeJson'] is returned if it failed to deserialize the sessions data.
    ///
    /// # Examples
    /// ```rust ignore
    /// use axum_session::{SessionNullPool, SessionConfig, SessionStore};
    /// use uuid::Uuid;
    ///
    /// let config = SessionConfig::default();
    /// let session_store = SessionStore::<SessionNullPool>::new(None, config);
    /// let token = Uuid::new_v4();
    /// async {
    ///     let session_data = session_store.load_session(token.to_string()).await.unwrap();
    /// };
    /// ```
    ///
    pub(crate) async fn load_session(
        &self,
        cookie_value: String,
    ) -> Result<Option<SessionData>, SessionError> {
        if let Some(client) = &self.client {
            let result: Option<String> =
                client.load(&cookie_value, &self.config.table_name).await?;

            Ok(result
                .map(|session| serde_json::from_str(&session))
                .transpose()?)
        } else {
            Ok(None)
        }
    }

    /// private internal function that stores a session's data to the database.
    ///
    /// If client is None it will return Ok(()).
    ///
    /// # Errors
    /// - ['SessionError::Sqlx'] is returned if database connection has failed or user does not have permissions.
    /// - ['SessionError::SerdeJson'] is returned if it failed to serialize the sessions data.
    ///
    /// # Examples
    /// ```rust ignore
    /// use axum_session::{SessionNullPool, SessionConfig, SessionStore, SessionData};
    /// use uuid::Uuid;
    ///
    /// let config = SessionConfig::default();
    /// let session_store = SessionStore::<SessionNullPool>::new(None, config.clone());
    /// let token = Uuid::new_v4();
    /// let session_data = SessionData::new(token, true, &config);
    ///
    /// async {
    ///     let _ = session_store.store_session(&session_data).await.unwrap();
    /// };
    /// ```
    ///
    pub(crate) async fn store_session(&self, session: &SessionData) -> Result<(), SessionError> {
        if let Some(client) = &self.client {
            client
                .store(
                    &session.id.to_string(),
                    &serde_json::to_string(session)?,
                    session.expires.unix_timestamp(),
                    &self.config.table_name,
                )
                .await?;
        }

        Ok(())
    }

    /// Deletes a session's data from the database by its UUID.
    ///
    /// If client is None it will return Ok(()).
    ///
    /// # Errors
    /// - ['SessionError::Sqlx'] is returned if database connection has failed or user does not have permissions.
    ///
    /// # Examples
    /// ```rust
    /// use axum_session::{SessionNullPool, SessionConfig, SessionStore};
    /// use uuid::Uuid;
    ///
    /// let config = SessionConfig::default();
    /// let session_store = SessionStore::<SessionNullPool>::new(None, config.clone());
    /// let token = Uuid::new_v4();
    ///
    /// async {
    ///     let _ = session_store.destroy_session(&token.to_string()).await.unwrap();
    /// };
    /// ```
    ///
    #[inline]
    pub async fn destroy_session(&self, id: &str) -> Result<(), SessionError> {
        if let Some(client) = &self.client {
            client.delete_one_by_id(id, &self.config.table_name).await?;
        }

        Ok(())
    }

    /// Deletes all sessions in the database.
    ///
    /// If client is None it will return Ok(()).
    ///
    /// # Errors
    /// - ['SessionError::Sqlx'] is returned if database connection has failed or user does not have permissions.
    ///
    /// # Examples
    /// ```rust
    /// use axum_session::{SessionNullPool, SessionConfig, SessionStore};
    /// use uuid::Uuid;
    ///
    /// let config = SessionConfig::default();
    /// let session_store = SessionStore::<SessionNullPool>::new(None, config.clone());
    ///
    /// async {
    ///     let _ = session_store.clear_store().await.unwrap();
    /// };
    /// ```
    ///
    #[inline]
    pub async fn clear_store(&self) -> Result<(), SessionError> {
        if let Some(client) = &self.client {
            client.delete_all(&self.config.table_name).await?;
        }

        Ok(())
    }

    /// Deletes all sessions in Memory.
    ///
    /// # Examples
    /// ```rust
    /// use axum_session::{SessionNullPool, SessionConfig, SessionStore};
    /// use uuid::Uuid;
    ///
    /// let config = SessionConfig::default();
    /// let session_store = SessionStore::<SessionNullPool>::new(None, config.clone());
    ///
    /// async {
    ///     let _ = session_store.clear_store().await.unwrap();
    /// };
    /// ```
    ///
    #[inline]
    pub fn clear(&self) {
        self.inner.clear();
    }

    /// Attempts to load check and clear Data.
    ///
    /// If no session is found returns false.
    pub(crate) fn service_session_data(&self, session: &Session<T>) -> bool {
        if let Some(mut inner) = self.inner.get_mut(&session.id.inner()) {
            if !inner.validate() || inner.destroy {
                inner.destroy = false;
                inner.longterm = false;
                inner.data.clear();
            }

            inner.autoremove = OffsetDateTime::now_utc() + self.config.memory_lifespan;
            return true;
        }

        false
    }

    #[inline]
    pub(crate) fn renew(&self, id: String) {
        if let Some(mut instance) = self.inner.get_mut(&id) {
            instance.renew();
        } else {
            tracing::warn!("Session data unexpectedly missing");
        }
    }

    #[inline]
    pub(crate) fn destroy(&self, id: String) {
        if let Some(mut instance) = self.inner.get_mut(&id) {
            instance.destroy();
        } else {
            tracing::warn!("Session data unexpectedly missing");
        }
    }

    #[inline]
    pub(crate) fn set_longterm(&self, id: String, longterm: bool) {
        if let Some(mut instance) = self.inner.get_mut(&id) {
            instance.set_longterm(longterm);
        } else {
            tracing::warn!("Session data unexpectedly missing");
        }
    }

    #[inline]
    pub(crate) fn set_store(&self, id: String, storable: bool) {
        if let Some(mut instance) = self.inner.get_mut(&id) {
            instance.set_store(storable);
        } else {
            tracing::warn!("Session data unexpectedly missing");
        }
    }

    #[inline]
    pub(crate) fn get<N: serde::de::DeserializeOwned>(&self, id: String, key: &str) -> Option<N> {
        if let Some(instance) = self.inner.get_mut(&id) {
            instance.get(key)
        } else {
            tracing::warn!("Session data unexpectedly missing");
            None
        }
    }

    #[inline]
    pub(crate) fn get_remove<N: serde::de::DeserializeOwned>(
        &self,
        id: String,
        key: &str,
    ) -> Option<N> {
        if let Some(mut instance) = self.inner.get_mut(&id) {
            instance.get_remove(key)
        } else {
            tracing::warn!("Session data unexpectedly missing");
            None
        }
    }

    #[inline]
    pub(crate) fn set(&self, id: String, key: &str, value: impl Serialize) {
        if let Some(mut instance) = self.inner.get_mut(&id) {
            instance.set(key, value);
        } else {
            tracing::warn!("Session data unexpectedly missing");
        }
    }

    #[inline]
    pub(crate) fn remove(&self, id: String, key: &str) {
        if let Some(mut instance) = self.inner.get_mut(&id) {
            instance.remove(key);
        } else {
            tracing::warn!("Session data unexpectedly missing");
        }
    }

    #[inline]
    pub(crate) fn clear_session_data(&self, id: String) {
        if let Some(mut instance) = self.inner.get_mut(&id) {
            instance.clear();
        } else {
            tracing::warn!("Session data unexpectedly missing");
        }
    }

    #[inline]
    pub(crate) async fn count_sessions(&self) -> i64 {
        if self.is_persistent() {
            self.count().await.unwrap_or(0i64)
        } else {
            self.inner.len() as i64
        }
    }
}
