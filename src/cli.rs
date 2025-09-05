use clap::Parser;

/// QQ音乐信息读取器 - 实时获取QQ音乐播放信息
#[derive(Parser, Debug)]
#[command(name = "QQMusic Reader")]
#[command(version = "1.0")]
#[command(about = "实时读取QQ音乐播放信息并输出到文件", long_about = None)]
#[command(disable_version_flag = true)]
#[command(disable_help_flag = true)]
pub struct Cli {
    /// 配置文件路径
    #[arg(short, long, default_value = "config.toml")]
    pub config: String,

    /// 启用调试模式（输出详细的内存读取信息）
    #[arg(short, long)]
    pub debug: bool,

    /// 禁用文本文件输出
    #[arg(long)]
    pub no_txt: bool,

    /// 禁用JSON文件输出
    #[arg(long)]
    pub no_json: bool,

    /// 自定义文本输出文件名
    #[arg(long, default_value = "now_playing.txt")]
    pub txt_file: String,

    /// 自定义JSON输出文件名
    #[arg(long, default_value = "now_playing.json")]
    pub json_file: String,

    /// 更新间隔（毫秒）
    #[arg(short, long, default_value_t = 500)]
    pub interval: u64,

    /// 最大重试次数
    #[arg(short, long, default_value_t = 3)]
    pub retries: u32,

    /// 进程名称（默认为QQMusic.exe）
    #[arg(long, default_value = "QQMusic.exe")]
    pub process: String,

    /// 模块名称（默认为QQMusic.dll）
    #[arg(long, default_value = "QQMusic.dll")]
    pub module: String,

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
        println!("      --no-txt            禁用文本文件输出");
        println!("      --no-json           禁用JSON文件输出");
        println!("      --txt-file <文件>   自定义文本输出文件名 (默认: now_playing.txt)");
        println!("      --json-file <文件>  自定义JSON输出文件名 (默认: now_playing.json)");
        println!("  -i, --interval <毫秒>   更新间隔 (默认: 500)");
        println!("  -r, --retries <次数>    最大重试次数 (默认: 3)");
        println!("      --process <名称>    进程名称 (默认: QQMusic.exe)");
        println!("      --module <名称>     模块名称 (默认: QQMusic.dll)");
        println!("  -q, --quiet             静默模式（不输出控制台信息）");
        println!("  -v, --version           显示版本信息");
        println!("  -h, --help              显示帮助信息");
        println!();
        println!("示例:");
        println!("  qqmusic-reader -d --interval 1000");
        println!("  qqmusic-reader --no-json --txt-file music_info.txt");
        println!("  qqmusic-reader -c custom_config.toml --process MyMusic.exe");
    }
}
