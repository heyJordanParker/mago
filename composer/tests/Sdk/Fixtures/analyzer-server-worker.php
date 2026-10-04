<?php

declare(strict_types=1);

namespace Mago\Tests\Sdk\Fixtures;

use Mago\Sdk\Analyzer\AfterAnalysisContext;
use Mago\Sdk\Analyzer\AfterAnalysisHook;
use Mago\Sdk\Analyzer\BeforeAnalysisContext;
use Mago\Sdk\Analyzer\BeforeAnalysisHook;
use Mago\Sdk\Analyzer\Plugin;
use Mago\Sdk\Analyzer\PluginDefinition;
use Mago\Sdk\Analyzer\PluginRegistry;
use Mago\Sdk\Extension;
use Mago\Sdk\Reporting\Issue;
use Mago\Sdk\Reporting\Level;
use Mago\Sdk\SourceLocation;
use Mago\Sdk\Span;
use Mago\Sdk\Worker;
use RuntimeException;

use function dirname;
use function preg_match_all;
use function strlen;
use function usort;

use const PREG_OFFSET_CAPTURE;
use const PREG_SET_ORDER;

require_once dirname(__DIR__, 4) . '/vendor/autoload.php';

/**
 * The hooks the analysis server tests drive: a before-analysis issue, a failing after-analysis
 * hook, and a cross-file rule that reports a route declared by two analyzed files.
 *
 * @mago-expect lint:file-name
 */
final class ServerProofPlugin implements Plugin, BeforeAnalysisHook, AfterAnalysisHook
{
    public function getDefinition(): PluginDefinition
    {
        return new PluginDefinition('server-proof', 'Server proof', 'Hooks the analysis server tests drive.');
    }

    public function register(PluginRegistry $registry): void
    {
        $registry->registerBeforeAnalysisHook($this);
        $registry->registerAfterAnalysisHook($this);
    }

    public function beforeAnalysis(BeforeAnalysisContext $context): void
    {
        $marker = $context->codebase->getConstant('PROOF_BEFORE');
        if ($marker !== null) {
            $context->report(Level::Warning, 'before', Issue::at('Before-analysis hook ran.', $marker->location));
        }
    }

    public function afterAnalysis(AfterAnalysisContext $context): void
    {
        if ($context->codebase->getConstant('PROOF_FAIL') !== null) {
            throw new RuntimeException('The after-analysis hook was told to fail.');
        }

        $files = $context->analysis->files;
        usort($files, static fn($left, $right): int => $left->file <=> $right->file);

        $owners = [];
        foreach ($files as $file) {
            preg_match_all('/route: (\S+)/', $file->getSourceFile()->contents, $matches, PREG_SET_ORDER | PREG_OFFSET_CAPTURE);
            foreach ($matches as [, [$route, $offset]]) {
                $owner = $owners[$route] ?? null;
                if ($owner === null) {
                    $owners[$route] = $file->file;
                    continue;
                }

                $location = new SourceLocation($file->file, new Span($offset, $offset + strlen($route)));
                $context->report(
                    Level::Error,
                    'duplicate-route',
                    Issue::at("Route `{$route}` is also declared in `{$owner}`.", $location),
                );
            }
        }
    }
}

(new Worker(new Extension(
    identifier: 'mago/server-proof',
    name: 'Mago analysis server proof',
    version: '1.0.0',
    analyzerPlugins: [new ServerProofPlugin()],
)))->run();
