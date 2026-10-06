<?php

declare(strict_types=1);

namespace Mago\Sdk\Analyzer;

/**
 * Runs once before parallel file analysis begins.
 *
 * A worker serves many analyses and runs this hook on the instance the plugin registered every
 * time, so state the hook keeps in its properties carries into the next analysis. A hook rebuilds
 * such state from the context on every call.
 *
 * @api
 */
interface BeforeAnalysisHook
{
    public function beforeAnalysis(BeforeAnalysisContext $context): void;
}
