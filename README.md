## 如何使用

1. 使用Releases的成品
2. 自行编译，需要Rust工具链

## 项目架构
- `darts/`
    对 `darts.h` 的移植，底层的双数组存储引擎

- `gram-db/` 
    对 `librime-octagram` 相关代码功能移植，对存储容器的包装

- `tool-cli/`
    一个简单的命令行程序，可以构建、导出、查询一个 `gram` 文件
    使用 `--help` 查看子命令与帮助
