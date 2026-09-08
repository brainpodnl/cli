use std::fmt;
use std::io::{self, IsTerminal as _, Write as _};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use serde_json::json;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::{TcpStream, tcp};
use tokio::sync::mpsc;
use tokio::task::JoinSet;
use tokio_stream::wrappers::ReceiverStream;
use tonic::metadata::MetadataValue;
use tonic::transport::{Channel, ClientTlsConfig, Endpoint};
use tonic::{Request, Streaming};
use uuid::Uuid;

use crate::client::Client;
use crate::cmd::TunnelArgs;
use crate::output::{CommandOutput, View};

mod pb {
    tonic::include_proto!("brainpod.tunnel.v1");
}

use pb::tunnel_broker_client::TunnelBrokerClient;
use pb::tunnel_service_client::TunnelServiceClient;
use pb::{Chunk, CloseSessionRequest, DatabaseEngine, OpenSessionRequest};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_CHUNK_BYTES: usize = 64 * 1024;
const INCLUDE_CREDENTIALS_METADATA: &str = "brainpod-include-credentials";
const DATABASE_PASSWORD_METADATA: &str = "brainpod-database-password-bin";

#[derive(Debug)]
struct RemoteTunnelError {
    operation: &'static str,
    code: Option<tonic::Code>,
    message: String,
    ends_tunnel: bool,
}

impl RemoteTunnelError {
    fn status(operation: &'static str, status: tonic::Status) -> Self {
        let code = status.code();
        Self {
            operation,
            code: Some(code),
            message: status.message().to_owned(),
            ends_tunnel: matches!(
                code,
                tonic::Code::Unauthenticated
                    | tonic::Code::PermissionDenied
                    | tonic::Code::NotFound
                    | tonic::Code::DeadlineExceeded
                    | tonic::Code::InvalidArgument
                    | tonic::Code::FailedPrecondition
            ),
        }
    }

    fn message(operation: &'static str, message: impl Into<String>) -> Self {
        Self {
            operation,
            code: None,
            message: message.into(),
            ends_tunnel: false,
        }
    }

    fn credentials(message: impl Into<String>) -> Self {
        Self {
            operation: "tunnel credentials unavailable",
            code: None,
            message: message.into(),
            ends_tunnel: true,
        }
    }
}

impl fmt::Display for RemoteTunnelError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.operation, self.message)?;
        if let Some(code) = self.code {
            write!(formatter, " ({code:?})")?;
        }
        Ok(())
    }
}

impl std::error::Error for RemoteTunnelError {}

pub async fn handle(
    client: &Client,
    pod: &str,
    args: TunnelArgs,
    control_plane_endpoint: &str,
    api_token: &str,
    json_output: bool,
) -> Result<CommandOutput> {
    let database_id = client
        .resolve_resource(pod, &args.resource)
        .await?
        .uuid
        .parse::<Uuid>()
        .context("Brainpod API returned an invalid resource UUID")?;

    let channel = connect_channel(control_plane_endpoint)
        .await
        .context("failed to connect to the Brainpod tunnel broker")?;
    let mut broker = TunnelBrokerClient::new(channel);
    let mut open_request = Request::new(OpenSessionRequest {
        database_id: database_id.to_string(),
        request_id: Uuid::new_v4().to_string(),
    });
    open_request
        .metadata_mut()
        .insert("authorization", bearer_value(api_token)?);
    let session = broker
        .open_session(open_request)
        .await
        .map_err(|status| RemoteTunnelError::status("failed to create tunnel session", status))?
        .into_inner();
    let engine = DatabaseEngine::try_from(session.engine)
        .ok()
        .filter(|engine| *engine != DatabaseEngine::Unspecified)
        .ok_or_else(|| anyhow!("Brainpod tunnel broker returned an unknown database engine"))?;
    let listen_address = args
        .listen_address
        .unwrap_or_else(|| SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), default_port(engine)));

    let proxy_result = run_proxy(
        listen_address,
        &session.endpoint,
        &session.ticket,
        engine,
        json_output,
        args.skip_preflight,
    )
    .await;

    let mut close_request = Request::new(CloseSessionRequest {
        session_id: session.session_id,
    });
    close_request
        .metadata_mut()
        .insert("authorization", bearer_value(api_token)?);
    let close_result = broker
        .close_session(close_request)
        .await
        .map_err(|status| RemoteTunnelError::status("failed to close tunnel session", status));

    let bound_address = match (proxy_result, close_result) {
        (Err(error), _) => return Err(error),
        (Ok(_), Err(error)) => return Err(error.into()),
        (Ok(bound_address), Ok(_)) => bound_address,
    };

    Ok(CommandOutput::new(
        json!({
            "event": "closed",
            "address": bound_address,
            "engine": engine_name(engine),
        }),
        View::Tunnel,
    ))
}

async fn run_proxy(
    listen_address: SocketAddr,
    endpoint: &str,
    ticket: &str,
    engine: DatabaseEngine,
    json_output: bool,
    skip_preflight: bool,
) -> Result<SocketAddr> {
    let listener = tokio::net::TcpListener::bind(listen_address)
        .await
        .with_context(|| format!("failed to bind tunnel listener at {listen_address}"))?;
    let listen_address = listener
        .local_addr()
        .context("failed to determine tunnel listener address")?;
    let channel = connect_channel(endpoint)
        .await
        .context("failed to connect to the Brainpod tunnel service")?;
    let client = TunnelServiceClient::new(channel);
    let ticket = ticket.to_owned();
    let password = if skip_preflight {
        None
    } else {
        let mut connection = open_remote(client.clone(), ticket.clone(), true).await?;
        let password = connection
            .password
            .take()
            .ok_or_else(|| RemoteTunnelError::credentials("remote service omitted the password"))?;
        drop(connection);
        Some(password)
    };
    announce(listen_address, engine, password.as_deref(), json_output)?;
    let mut connections = JoinSet::new();
    let shutdown = tokio::signal::ctrl_c();
    tokio::pin!(shutdown);

    loop {
        tokio::select! {
            result = &mut shutdown => {
                result.context("failed to listen for Ctrl-C")?;
                break;
            }
            accepted = listener.accept() => {
                let (stream, peer) = accepted.context("failed to accept tunnel connection")?;
                write_connection_event(peer, ConnectionEvent::Opened, json_output)?;
                let client = client.clone();
                let ticket = ticket.clone();
                connections.spawn(async move {
                    (peer, relay(stream, client, ticket).await)
                });
            }
            result = connections.join_next(), if !connections.is_empty() => {
                if let Some(result) = result {
                    let (peer, result) = result.context("tunnel connection task failed")?;
                    match result {
                        Ok(()) => write_connection_event(peer, ConnectionEvent::Closed, json_output)?,
                        Err(error) => {
                            let message = connection_error_message(&error);
                            write_connection_event(
                                peer,
                                ConnectionEvent::Failed(&message),
                                json_output,
                            )?;
                            if error
                                .downcast_ref::<RemoteTunnelError>()
                                .is_some_and(|error| error.ends_tunnel)
                            {
                                return Err(anyhow!("tunnel closed: {message}"));
                            }
                        }
                    }
                }
            }
        }
    }

    connections.abort_all();
    while connections.join_next().await.is_some() {}
    Ok(listen_address)
}

struct RemoteConnection {
    sender: mpsc::Sender<Chunk>,
    inbound: Streaming<Chunk>,
    password: Option<String>,
}

async fn relay(
    stream: TcpStream,
    client: TunnelServiceClient<Channel>,
    ticket: String,
) -> Result<()> {
    let remote = open_remote(client, ticket, false).await?;
    relay_open(stream, remote).await
}

async fn open_remote(
    mut client: TunnelServiceClient<Channel>,
    ticket: String,
    include_credentials: bool,
) -> Result<RemoteConnection> {
    let (sender, receiver) = mpsc::channel(16);
    let mut request = Request::new(ReceiverStream::new(receiver));
    request
        .metadata_mut()
        .insert("authorization", bearer_value(&ticket)?);
    if include_credentials {
        request
            .metadata_mut()
            .insert(INCLUDE_CREDENTIALS_METADATA, "true".parse()?);
    }
    let response = client.open(request).await.map_err(|status| {
        RemoteTunnelError::status("tunnel service rejected the connection", status)
    })?;
    let password = if include_credentials {
        let value = response
            .metadata()
            .get_bin(DATABASE_PASSWORD_METADATA)
            .ok_or_else(|| RemoteTunnelError::credentials("remote service omitted the password"))?;
        let bytes = value.to_bytes().map_err(|_| {
            RemoteTunnelError::credentials("remote service returned an invalid password")
        })?;
        Some(String::from_utf8(bytes.to_vec()).map_err(|_| {
            RemoteTunnelError::credentials("remote service returned an invalid password")
        })?)
    } else {
        None
    };
    Ok(RemoteConnection {
        sender,
        inbound: response.into_inner(),
        password,
    })
}

async fn relay_open(stream: TcpStream, remote: RemoteConnection) -> Result<()> {
    let (reader, writer) = stream.into_split();
    tokio::try_join!(
        upload(reader, remote.sender),
        download(writer, remote.inbound)
    )?;
    Ok(())
}

async fn upload(mut reader: tcp::OwnedReadHalf, sender: mpsc::Sender<Chunk>) -> Result<()> {
    let mut buffer = vec![0; MAX_CHUNK_BYTES];
    loop {
        let read = reader.read(&mut buffer).await?;
        if read == 0 {
            return Ok(());
        }
        sender
            .send(Chunk {
                data: buffer[..read].to_vec(),
            })
            .await
            .map_err(|_| {
                RemoteTunnelError::message("tunnel upload failed", "remote stream closed")
            })?;
    }
}

async fn download(mut writer: tcp::OwnedWriteHalf, mut inbound: Streaming<Chunk>) -> Result<()> {
    while let Some(chunk) = inbound
        .message()
        .await
        .map_err(|status| RemoteTunnelError::status("tunnel download failed", status))?
    {
        if chunk.data.is_empty() {
            return Err(RemoteTunnelError::message(
                "tunnel download failed",
                "remote service returned an empty chunk",
            )
            .into());
        }
        writer.write_all(&chunk.data).await?;
    }
    writer.shutdown().await?;
    Ok(())
}

async fn connect_channel(endpoint: &str) -> Result<Channel> {
    crate::install_crypto_provider()?;
    let endpoint_builder = Endpoint::from_shared(endpoint.to_owned())?
        .connect_timeout(CONNECT_TIMEOUT)
        .tcp_keepalive(Some(Duration::from_secs(30)))
        .http2_keep_alive_interval(Duration::from_secs(30))
        .keep_alive_timeout(Duration::from_secs(20))
        .keep_alive_while_idle(true);
    let endpoint_builder = if endpoint.starts_with("https://") {
        endpoint_builder.tls_config(ClientTlsConfig::new().with_enabled_roots())?
    } else {
        endpoint_builder
    };
    Ok(endpoint_builder.connect().await?)
}

fn announce(
    address: SocketAddr,
    engine: DatabaseEngine,
    password: Option<&str>,
    json_output: bool,
) -> Result<()> {
    let details = connection_details(address, engine, password);
    let stdout = io::stdout();
    let mut stdout = stdout.lock();
    if json_output {
        writeln!(
            stdout,
            "{}",
            serde_json::to_string(&json!({
                "event": "listening",
                "address": address,
                "host": address.ip(),
                "localPort": address.port(),
                "remotePort": details.remote_port,
                "engine": engine_name(engine),
                "clientCommand": details.client_command,
            }))?
        )
        .context("failed to write tunnel address")?;
        if let Some(password) = password {
            writeln!(
                stdout,
                "{}",
                serde_json::to_string(&json!({
                    "event": "credentials",
                    "username": details.username,
                    "database": details.database,
                    "password": password,
                    "dsn": details.dsn,
                }))?
            )
            .context("failed to write database credentials")?;
        }
    } else {
        let color = stdout.is_terminal();
        let title = style("◆ Brainpod tunnel", "1;35", color);
        let engine = style(details.display_name, "1;36", color);
        let ready = style("● Ready", "1;32", color);
        let label = |value: &str| style(&format!("{value:<10}"), "2", color);

        writeln!(stdout, "╭─ {title}")?;
        writeln!(stdout, "│")?;
        writeln!(stdout, "│  {engine}")?;
        writeln!(stdout, "│  {} {address}", label("Local"))?;
        writeln!(
            stdout,
            "│  {} {}:{}",
            label("Remote"),
            details.display_name,
            details.remote_port
        )?;
        if details.username.is_some() || details.database.is_some() || password.is_some() {
            writeln!(stdout, "│")?;
        }
        if let Some(username) = details.username {
            writeln!(stdout, "│  {} {username}", label("Username"))?;
        }
        if let Some(database) = details.database {
            writeln!(stdout, "│  {} {database}", label("Database"))?;
        }
        if let Some(password) = password {
            writeln!(stdout, "│  {} {password}", label("Password"))?;
        }
        writeln!(stdout, "│")?;
        writeln!(stdout, "├─ {}", style("Client", "1", color))?;
        writeln!(stdout, "│  {}", details.client_command)?;
        if let Some(dsn) = details.dsn {
            writeln!(stdout, "│")?;
            writeln!(stdout, "├─ {}", style("DSN", "1", color))?;
            writeln!(stdout, "│  {dsn}")?;
        }
        writeln!(stdout, "│")?;
        writeln!(stdout, "╰─ {ready} · press Ctrl+C to stop")?;
    }
    stdout.flush().context("failed to flush tunnel banner")
}

enum ConnectionEvent<'a> {
    Opened,
    Closed,
    Failed(&'a str),
}

fn write_connection_event(
    peer: SocketAddr,
    event: ConnectionEvent<'_>,
    json_output: bool,
) -> Result<()> {
    if json_output {
        let (event, error) = match event {
            ConnectionEvent::Opened => ("connectionOpened", None),
            ConnectionEvent::Closed => ("connectionClosed", None),
            ConnectionEvent::Failed(error) => ("connectionFailed", Some(error)),
        };
        let stdout = io::stdout();
        let mut stdout = stdout.lock();
        writeln!(
            stdout,
            "{}",
            serde_json::to_string(&json!({
                "event": event,
                "peer": peer,
                "error": error,
            }))?
        )?;
        stdout.flush()?;
        return Ok(());
    }

    let stderr = io::stderr();
    let color = stderr.is_terminal();
    let mut stderr = stderr.lock();
    match event {
        ConnectionEvent::Opened => {
            writeln!(stderr, "{} {peer}  connected", style("→", "1;36", color))?
        }
        ConnectionEvent::Closed => {
            writeln!(stderr, "{} {peer}  closed", style("✓", "1;32", color))?
        }
        ConnectionEvent::Failed(error) => {
            writeln!(stderr, "{} {peer}  {error}", style("×", "1;31", color))?
        }
    }
    stderr.flush()?;
    Ok(())
}

fn connection_error_message(error: &anyhow::Error) -> String {
    error.downcast_ref::<RemoteTunnelError>().map_or_else(
        || error.root_cause().to_string(),
        |error| error.message.clone(),
    )
}

fn style(value: &str, code: &str, enabled: bool) -> String {
    if enabled {
        format!("\u{1b}[{code}m{value}\u{1b}[0m")
    } else {
        value.to_owned()
    }
}

struct ConnectionDetails {
    display_name: &'static str,
    remote_port: u16,
    username: Option<&'static str>,
    database: Option<&'static str>,
    client_command: String,
    dsn: Option<String>,
}

fn connection_details(
    address: SocketAddr,
    engine: DatabaseEngine,
    password: Option<&str>,
) -> ConnectionDetails {
    let host = address.ip();
    let dsn_host = dsn_host(host);
    let port = address.port();
    let password = password.map(percent_encode);
    match engine {
        DatabaseEngine::Postgres => ConnectionDetails {
            display_name: "PostgreSQL",
            remote_port: 5432,
            username: Some("brainpod"),
            database: Some("brainpod"),
            client_command: format!(
                "psql \"host={host} port={port} user=brainpod dbname=brainpod sslmode=require\""
            ),
            dsn: password.map(|password| {
                format!(
                    "postgres://brainpod:{password}@{dsn_host}:{port}/brainpod?sslmode=require"
                )
            }),
        },
        DatabaseEngine::Mariadb => ConnectionDetails {
            display_name: "MariaDB",
            remote_port: 3306,
            username: Some("brainpod"),
            database: Some("brainpod"),
            client_command: format!(
                "mariadb --ssl --host {host} --port {port} --user brainpod --password brainpod"
            ),
            dsn: password.map(|password| {
                format!("mysql://brainpod:{password}@{dsn_host}:{port}/brainpod")
            }),
        },
        DatabaseEngine::Valkey => ConnectionDetails {
            display_name: "Valkey",
            remote_port: 6379,
            username: None,
            database: None,
            client_command: format!("valkey-cli --tls --insecure -h {host} -p {port}"),
            dsn: password
                .map(|password| format!("rediss://:{password}@{dsn_host}:{port}")),
        },
        DatabaseEngine::Mssql => ConnectionDetails {
            display_name: "Microsoft SQL Server",
            remote_port: 1433,
            username: Some("brainpod"),
            database: Some("brainpod"),
            client_command: format!(
                "sqlcmd -S tcp:{host},{port} -U brainpod -d brainpod -C"
            ),
            dsn: password.map(|password| {
                format!(
                    "sqlserver://brainpod:{password}@{dsn_host}:{port}?database=brainpod&encrypt=true&trustServerCertificate=true"
                )
            }),
        },
        DatabaseEngine::Unspecified => ConnectionDetails {
            display_name: "Database",
            remote_port: 0,
            username: None,
            database: None,
            client_command: address.to_string(),
            dsn: None,
        },
    }
}

fn dsn_host(host: IpAddr) -> String {
    match host {
        IpAddr::V4(host) => host.to_string(),
        IpAddr::V6(host) => format!("[{host}]"),
    }
}

fn percent_encode(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            encoded.push(char::from(byte));
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

fn bearer_value(token: &str) -> Result<MetadataValue<tonic::metadata::Ascii>> {
    format!("Bearer {token}")
        .parse()
        .context("API token contains invalid header characters")
}

const fn default_port(engine: DatabaseEngine) -> u16 {
    match engine {
        DatabaseEngine::Postgres => 5432,
        DatabaseEngine::Mariadb => 3306,
        DatabaseEngine::Valkey => 6379,
        DatabaseEngine::Mssql => 1433,
        DatabaseEngine::Unspecified => 0,
    }
}

const fn engine_name(engine: DatabaseEngine) -> &'static str {
    match engine {
        DatabaseEngine::Postgres => "postgres",
        DatabaseEngine::Mariadb => "mariadb",
        DatabaseEngine::Valkey => "valkey",
        DatabaseEngine::Mssql => "mssql",
        DatabaseEngine::Unspecified => "unspecified",
    }
}

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;

    use super::{DatabaseEngine, RemoteTunnelError, connection_details};

    #[test]
    fn formats_remote_status_without_metadata() {
        let error = RemoteTunnelError::status(
            "tunnel service rejected the connection",
            tonic::Status::unavailable("database unavailable"),
        );

        assert_eq!(
            error.to_string(),
            "tunnel service rejected the connection: database unavailable (Unavailable)"
        );
        assert!(!error.ends_tunnel);
    }

    #[test]
    fn ends_expired_or_invalid_sessions() {
        for status in [
            tonic::Status::unauthenticated("invalid tunnel credentials"),
            tonic::Status::deadline_exceeded("tunnel session expired"),
            tonic::Status::failed_precondition("database credentials unavailable"),
        ] {
            assert!(RemoteTunnelError::status("connection failed", status).ends_tunnel);
        }
    }

    #[test]
    fn builds_postgres_banner_details() {
        let address = "127.0.0.1:15432".parse::<SocketAddr>().unwrap();
        let details = connection_details(address, DatabaseEngine::Postgres, Some("p@ ss"));

        assert_eq!(details.display_name, "PostgreSQL");
        assert_eq!(details.remote_port, 5432);
        assert_eq!(details.username, Some("brainpod"));
        assert_eq!(details.database, Some("brainpod"));
        assert_eq!(
            details.dsn.as_deref(),
            Some("postgres://brainpod:p%40%20ss@127.0.0.1:15432/brainpod?sslmode=require")
        );
        assert!(details.client_command.starts_with("psql "));
    }

    #[test]
    fn omits_dsn_without_preflight_credentials() {
        let address = "127.0.0.1:16379".parse::<SocketAddr>().unwrap();
        let details = connection_details(address, DatabaseEngine::Valkey, None);

        assert_eq!(details.remote_port, 6379);
        assert!(details.dsn.is_none());
    }
}
