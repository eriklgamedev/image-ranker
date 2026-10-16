const capacitySpec = './tests/desktop/decision-capacity.e2e.mjs';

// Extended time belongs to this explicit qualification workload only. Normal
// desktop acceptance retains its existing timeout and workflow selection.
export function desktopProfile(profile, args = []) {
  if (profile === undefined || profile === '') {
    return {name: 'standard', specs: ['./tests/desktop/workflow.e2e.mjs'], timeoutMs: 180000};
  }
  if (profile !== 'decision-capacity') throw new Error('Unknown desktop qualification profile');
  const requested = [];
  for (let index = 0; index < args.length; index++) {
    if (args[index] === '--spec') {
      if (!args[index + 1] || args[index + 1].startsWith('--')) throw new Error('Missing qualification spec');
      requested.push(args[++index]);
    } else if (args[index].startsWith('--spec=')) {
      requested.push(args[index].slice('--spec='.length));
    }
  }
  if (requested.some(spec => spec.replaceAll('\\', '/').replace(/^\.\//, '') !== capacitySpec.slice(2))) {
    throw new Error('Decision capacity profile cannot extend another desktop spec');
  }
  return {name: profile, specs: [capacitySpec], timeoutMs: 1800000};
}
