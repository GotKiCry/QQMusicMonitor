use clap::Parser;

/// QQ音乐信息读取器 - 实时获取QQ音乐播放信息
#[derive(Parser, Debug, Clone)]
#[command(name = "QQMusic Reader")]
#[command(version = "1.0")]
#[command(about = "实时读取QQ音乐播放信息并通过窗口/同步服务展示", long_about = None)]
#[command(disable_version_flag = true)]
#[command(disable_help_flag = true)]
pub struct Cli {
    /// 配置文件路径
    #[arg(short, long, default_value = "config.toml")]
    pub config: String,

    /// 启用调试模式（输出详细的内存读取信息）
    #[arg(short, long)]
    pub debug: bool,

    /// 禁用本地同步服务
    #[arg(long)]
    pub no_server: bool,

    /// 更新间隔（毫秒），不指定则使用配置文件
    #[arg(short, long)]
    pub interval: Option<u64>,

    /// SMTC 滞后补偿量（毫秒），不指定则使用配置文件
    #[arg(short, long)]
    pub offset: Option<i64>,

    /// 同步服务端口
    #[arg(long, default_value_t = 3000)]
    pub port: u16,

    /// 最大重试次数
    #[arg(short, long, default_value_t = 3)]
    pub retries: u32,

    /// 静默模式（不输出控制台信息）
    #[arg(short, long)]
    pub quiet: bool,


    /// 显示版本信息
    #[arg(long)]
    pub version: bool,

    /// 显示帮助信息
    #[arg(short, long)]
    pub help: bool,
}

impl Cli {
    /// 解析命令行参数
    pub fn parse_args() -> Self {
        Self::parse()
    }

    /// 显示版本信息
    pub fn show_version() {
        println!("QQMusic Reader v{}", env!("CARGO_PKG_VERSION"));
        println!("作者: QQMusic Reader 开发团队");
        println!("许可证: MIT");
    }

    /// 显示帮助信息
    pub fn show_help() {
        println!("QQMusic Reader - 实时读取QQ音乐播放信息");
        println!();
        println!("使用方法: qqmusic-reader [选项]");
        println!();
        println!("选项:");
        println!("  -c, --config <文件>     配置文件路径 (默认: config.toml)");
        println!("  -d, --debug             启用调试模式");
        println!("      --no-server         禁用本地同步服务");
        println!("  -i, --interval <毫秒>   更新间隔 (不指定则使用配置文件)");
        println!("  -o, --offset <毫秒>     SMTC 滞后补偿量 (不指定则使用配置文件)");
        println!("      --port <端口>       自定义同步服务端口 (默认: 3000)");
        println!("  -r, --retries <次数>    最大重试次数 (默认: 3)");
        println!("  -q, --quiet             静默模式（不输出控制台信息）");

        println!("  -v, --version           显示版本信息");
        println!("  -h, --help              显示帮助信息");
        println!();
        println!("示例:");
        println!("  qqmusic-reader -d --interval 1000");
        println!("  qqmusic-reader -c custom_config.toml");
    }
}
