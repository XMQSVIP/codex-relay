import type { AccountPolicy, Group } from './api';

export const accountGroupIds = (policy?: AccountPolicy): string[] => policy ? policy.groupIds : [];
export const sameGroups = (left: string[], right: string[]) => left.length === right.length && left.every(id => right.includes(id));
export const availableAccountGroup = (policy: AccountPolicy | undefined, groups: Group[]) => {
  const memberships = groups.filter(group => accountGroupIds(policy).includes(group.id));
  return memberships.find(group => group.enabled) ?? memberships[0];
};
