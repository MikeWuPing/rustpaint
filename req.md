项目目的：做一个UEFI Shell下GUI的类似Windows小画家的app。要求：
1. 必须采用Rust来写，可以参考Patina方案的demo啥的
2. 使用emulator-uefi-shell-app进行app开发。edk2在上级目录已经下载，编译工具和QEMU已经安装，不要重复下载和安装
3. 支持鼠标和键盘，在qemu下进行验证。键盘支持tab键在各个窗口切换焦点，焦点点亮的项目highlight，失焦的lowlight,就像一般Windows app那样。app的菜单和对话框也类似Windows 11的效果。
4. app建立自己的目录，组织形式可以参考上一级目录下面的advmemtest目录和guedit。后期我会在github上建个私仓。
5. 英文菜单即可。
6. 功能要求：要有小画家的基础功能即可。
