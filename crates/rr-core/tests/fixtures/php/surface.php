<?php

namespace App;

class Service implements Contract
{
    public const MAX = 3;
    private int $secret;

    public function __construct(string $name)
    {
        $this->name = $name;
    }

    public function run(): void
    {
        helper();
    }

    private function hidden(): void
    {
    }
}

interface Contract
{
    function run(): void;
}

function bare(string $name): string
{
    return $name;
}
