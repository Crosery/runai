pub mod tools;

use anyhow::Result;

pub async fn serve() -> Result<()> {
    let transport = rmcp::transport::io::stdio();
    let server = tools::SmServer::new()?;
    let service = rmcp::serve_server(server, transport).await?;
    service.waiting().await?;
    Ok(())
}
