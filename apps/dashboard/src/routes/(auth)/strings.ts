import { extend } from "#lib/i18n.ts";

extend({
  // ---- 登录 ----
  "login.title": ["登录 Rivu", "Sign in to Rivu"],
  "login.email": ["邮箱", "Email"],
  "login.password": ["密码", "Password"],
  "login.submit": ["登录", "Sign in"],
  "login.showPassword": ["显示密码", "Show password"],
  "login.hidePassword": ["隐藏密码", "Hide password"],
  "login.invalid": ["邮箱或密码不正确。", "Email or password is incorrect."],
  "login.required": ["请填写邮箱和密码。", "Enter your email and password."],
  "login.rate": ["尝试次数过多，请在 {n} 秒后重试。", "Too many attempts. Try again in {n} seconds."],
  "login.rate.wait": ["{n} 秒后重试", "Retry in {n}s"],
  "login.network": ["无法连接到服务器。", "Can't reach the server."],
  "login.failed": ["登录失败，请稍后重试。", "Sign-in failed. Try again shortly."],
  "login.help": ["没有账号或忘记密码，请联系管理员。", "No account or forgot your password? Contact an admin."],

  // ---- 初始化 ----
  "setup.title": ["初始化 Rivu", "Set up Rivu"],
  "setup.token": ["初始化令牌", "Setup token"],
  "setup.token.invalid": ["初始化令牌不正确。", "The setup token is incorrect."],
  "setup.name": ["名字", "Name"],
  "setup.name.long": ["名字最多 100 个字符。", "Name must be at most 100 characters."],
  "setup.email": ["邮箱", "Email"],
  "setup.email.invalid": ["请填写有效的邮箱地址。", "Enter a valid email address."],
  "setup.password": ["密码", "Password"],
  "setup.password.hint": ["12 到 128 个字符。", "12 to 128 characters."],
  "setup.password.short": ["密码至少 12 个字符。", "Password must be at least 12 characters."],
  "setup.password.long": ["密码最多 128 个字符。", "Password must be at most 128 characters."],
  "setup.orgName": ["组织名称", "Organization name"],
  "setup.orgName.required": ["请填写组织名称，不超过 100 个字符。", "Enter an organization name, up to 100 characters."],
  "setup.submit": ["创建", "Create"],
  "setup.done": ["初始化完成", "Setup complete"],
  "setup.have": ["已经初始化过？", "Already set up?"],
  "setup.signIn": ["登录", "Sign in"],
  "setup.already": ["这个实例已经初始化过。", "This instance is already set up."],
});
