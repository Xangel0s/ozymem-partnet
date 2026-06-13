use ozymem_core::{
    default_memgraph_database, default_memgraph_uri, MemgraphConfig, MemgraphConnection,
};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let uri = std::env::var("MEMGRAPH_URI").unwrap_or_else(|_| default_memgraph_uri().to_string());
    let user = std::env::var("MEMGRAPH_USER")
        .expect("MEMGRAPH_USER environment variable is required. Set it to your Memgraph username.");
    let password = std::env::var("MEMGRAPH_PASSWORD")
        .expect("MEMGRAPH_PASSWORD environment variable is required. Set it to your Memgraph password.");
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
