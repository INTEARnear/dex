import { b } from "@zorsh/zorsh";

export const XykUpgradePoolArgsSchema = b.struct({
  pool_id: b.u32(),
});
export type XykUpgradePoolArgs = b.infer<typeof XykUpgradePoolArgsSchema>;
