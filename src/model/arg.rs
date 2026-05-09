use clap::Parser;

/// Anthropic <-> Kiro API 客户端
#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
pub struct Args {
    /// 配置文件路径
    #[arg(short, long)]
    pub config: Option<String>,

    /// 凭证文件路径
    #[arg(long)]
    pub credentials: Option<String>,

    /// 代理池文件路径（启用代理池时使用，未指定回退到 config.proxyPoolPath 或 ./proxies.json）
    #[arg(long)]
    pub proxies: Option<String>,

    /// SQLite 数据库文件路径（覆盖 config.dbPath；默认 ./kiro.db）
    #[arg(long)]
    pub db: Option<String>,
}
