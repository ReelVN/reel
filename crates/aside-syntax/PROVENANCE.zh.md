# Aside 语法库来源

[English](PROVENANCE.md)

初始实现于 2026-10-13 引入，源自 Rowan 0.17.0 的提交 `c781c336f44a28210fefaa4d86b8c9902b781c8e`。保留原有的 MIT 和 Apache-2.0 许可证及源码版权声明，内部 Arc 实现保留 Triomphe 的署名。[rust-analyzer/rowan](https://github.com/rust-analyzer/rowan)

Reel 以 `aside-syntax` 维护引入的库源码。本地改动使用显式队列销毁绿树，并以原子操作判定最后一个所有者。词法游标迭代遍历错误恢复子树，保留零长度记号和子树边界。节点缓存保留哈希值，避免哈希表扩容时重新遍历子树。
