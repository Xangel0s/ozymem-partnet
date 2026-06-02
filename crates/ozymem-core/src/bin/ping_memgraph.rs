use ozymem_core::{
    default_memgraph_database, default_memgraph_uri, MemgraphConfig, MemgraphConnection,
};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let uri = std::env::var("MEMGRAPH_URI").unwrap_or_else(|_| default_memgraph_uri().to_string());
    let user = std::env::var("MEMGRAPH_USER").unwrap_or_else(|_| "admin".to_string());
    let password = std::env::var("MEMGRAPH_PASSWORD").unwrap_or_else(|_| "admin".to_string());
    let database = std::env::var("MEMGRAPH_DATABASE")
        .unwrap_or_else(|_| default_memgraph_database().to_string());

    let connection = MemgraphConnection::connect(MemgraphConfig {
        uri,
        user,
        password,
        database,
    })
    .await?;
    let value = connection.ping().await?;

    println!("Memgraph responded with {value}");
    Ok(())
}
