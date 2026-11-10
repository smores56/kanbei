import { Effect, Option } from "effect"
import {
  describe,
  filesIn,
  interfaceNames,
  readSysctl,
  requireProgram,
  truncate,
} from "../facts.ts"
import { findings, probe, ProbeEnv } from "../probe.ts"
import { runUserUnit } from "../systemd-run.ts"

const maxUserNamespaces = probe(
  {
    id: "NS-01",
    category: "namespaces",
    arm: "both",
    method: "read /proc/sys/user/max_user_namespaces",
  },
  Effect.gen(function* () {
    const value = yield* readSysctl("user.max_user_namespaces")
    const count = Number(value)
    return findings(
      count > 0 ? "pass" : "fail",
      `user.max_user_namespaces=${value}`,
      "cat /proc/sys/user/max_user_namespaces",
      count > 0 ? undefined : "user namespaces disabled; namespaced sandbox units cannot start",
    )
  }),
)

const userNamespace = probe(
  {
    id: "NS-02",
    category: "namespaces",
    arm: "both",
    method: "unshare --user --map-root-user id -u",
    prerequisites: ["unshare"],
  },
  Effect.gen(function* () {
    const unshare = yield* requireProgram("unshare")
    const env = yield* ProbeEnv
    const result = yield* env.command.run(unshare, [
      "--user",
      "--map-root-user",
      "--",
      "id",
      "-u",
    ], { timeoutMs: 10_000 })
    const uid = result.stdout.trim()
    return findings(
      result.code === 0 && uid === "0" ? "pass" : "fail",
      result.code === 0 ? `uid inside user namespace = ${uid}` : "user namespace creation failed",
      `unshare --user --map-root-user id -u -> ${describe(result)}`,
    )
  }),
)

const networkNamespace = probe(
  {
    id: "NS-03",
    category: "namespaces",
    arm: "both",
    method: "unshare --user --map-root-user --net, enumerate interfaces",
    prerequisites: ["unshare"],
  },
  Effect.gen(function* () {
    const unshare = yield* requireProgram("unshare")
    const env = yield* ProbeEnv
    const result = yield* env.command.run(unshare, [
      "--user",
      "--map-root-user",
      "--net",
      "--",
      "sh",
      "-c",
      "cat /proc/net/dev",
    ], { timeoutMs: 10_000 })
    const hostInterfaces = yield* filesIn("/sys/class/net")
    const namespacedInterfaces = interfaceNames(result.stdout)
    const isolated = result.code === 0 && namespacedInterfaces.every((name) => name === "lo")
    return findings(
      isolated ? "pass" : "fail",
      isolated
        ? `network namespace contains only loopback (host has ${hostInterfaces.join(",")})`
        : `network namespace interfaces = ${namespacedInterfaces.join(",") || "none"}`,
      `unshare -Urn cat /proc/net/dev -> ${truncate(result.stdout.replaceAll("\n", " "))}`,
    )
  }),
)

const pidNamespace = probe(
  {
    id: "NS-04",
    category: "namespaces",
    arm: "both",
    method: "unshare --user --map-root-user --pid --fork",
    prerequisites: ["unshare"],
  },
  Effect.gen(function* () {
    const unshare = yield* requireProgram("unshare")
    const env = yield* ProbeEnv
    const result = yield* env.command.run(unshare, [
      "--user",
      "--map-root-user",
      "--pid",
      "--fork",
      "--",
      "sh",
      "-c",
      "echo pid=$$",
    ], { timeoutMs: 10_000 })
    return findings(
      result.code === 0 ? "pass" : "fail",
      result.code === 0 ? "pid namespace started" : "pid namespace creation failed",
      `unshare -Ur --pid --fork sh -c 'echo pid=$$' -> ${describe(result)}`,
    )
  }),
)

const mountNamespace = probe(
  {
    id: "NS-05",
    category: "namespaces",
    arm: "both",
    method: "unshare --user --map-root-user --mount, mount tmpfs in scratch",
    prerequisites: ["unshare", "mount"],
  },
  Effect.gen(function* () {
    const unshare = yield* requireProgram("unshare")
    const env = yield* ProbeEnv
    const target = env.path.join(env.scratch, "ns-mount")
    yield* env.fs.makeDirectory(target, { recursive: true })
    const result = yield* env.command.run(unshare, [
      "--user",
      "--map-root-user",
      "--mount",
      "--",
      "sh",
      "-c",
      `mount -t tmpfs tmpfs ${target} && touch ${target}/probe && echo mount-ok`,
    ], { timeoutMs: 10_000 })
    return findings(
      result.stdout.includes("mount-ok") ? "pass" : "fail",
      result.stdout.includes("mount-ok")
        ? "private mount namespace accepted a tmpfs mount"
        : "mount inside user namespace failed",
      `unshare -Urm mount -t tmpfs ... -> ${describe(result)}`,
      result.stdout.includes("mount-ok")
        ? undefined
        : "filesystem isolation in user units depends on this mount path",
    )
  }),
)

const privateUsersUnit = probe(
  {
    id: "NS-06",
    category: "namespaces",
    arm: "both",
    method: "systemd-run --user -p PrivateUsers=yes id -u",
    prerequisites: ["systemd-run", "systemctl"],
  },
  Effect.gen(function* () {
    const env = yield* ProbeEnv
    const id = yield* env.command.locate("id")
    if (Option.isNone(id)) {
      return findings("blocked", "id(1) is not installed", "")
    }
    const result = yield* runUserUnit({ properties: ["PrivateUsers=yes"] }, id.value, ["-u"])
    const uid = result.stdout.trim()
    const rejected = /Invalid|not supported|Failed to start|Unknown|Permission denied/u.test(
      result.stderr,
    )
    if (rejected) {
      return findings("blocked", "user manager rejected PrivateUsers=yes", describe(result))
    }
    return findings(
      result.code === 0 && uid !== "" && uid !== "1000" ? "pass" : "fail",
      result.code === 0
        ? `uid inside namespaced user unit = ${uid || "unknown"}`
        : "namespaced user unit did not start",
      `systemd-run --user -p PrivateUsers=yes id -u -> ${describe(result)}`,
      result.code === 0 && uid === "1000"
        ? "PrivateUsers=yes started without changing the UID mapping"
        : undefined,
    )
  }),
)

export const namespaceProbes = Effect.all(
  [
    maxUserNamespaces,
    userNamespace,
    networkNamespace,
    pidNamespace,
    mountNamespace,
    privateUsersUnit,
  ],
  { concurrency: 1 },
)
