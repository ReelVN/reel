# 源自 Quirrel 的执行用例

[English](README.md)

`original/` 保留了 Quirrel 提交 `19f17df9b6ee43c91ef5c635fd45970ec76c5dbe` 的原始输入、预期结果和测试运行器资料。已审查的 Aside 改写及其源码映射收录在 `provenance.json`，需要通过 Aside 自身契约覆盖的语言差异记在 `deliberate-deltas.json`。[许可证](LICENSE)

`semantic_quirrel.rs` 检查原始断言的词法记号和映射后的源码范围。`quirrel_vm.rs` 编译注册的用例，验证字节码往返，然后以正常和极小的执行时间片穿插垃圾回收来执行它们。结果与原始预期标准输出对照，逐行比较沿用原测试运行器的换行规则。字符串插值前缀的改动保留明确的源码映射。

```sh
cargo test --locked -p aside-compiler --test semantic_quirrel --test quirrel_vm
```

闭包和循环的改写保留嵌套捕获与同名遮蔽。顶层 `return` 的用例在显式函数里执行，代码块的局部返回改用块末尾表达式或局部结果变量，保留原有异常断言。Quirrel 编译器特有的诊断、AST 和优化文件保留作参考，注册的 Aside 用例覆盖目前维护的执行行为。
